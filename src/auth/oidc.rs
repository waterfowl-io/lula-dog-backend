//! The real [`TokenVerifier`]: a generic OIDC bearer-token validator.
//!
//! Configured entirely from `[auth]` -- issuer, audience, and key material --
//! and knows nothing about which vendor issued the token. Two key sources,
//! either or both active at once, chosen per token by the JWT header `alg`:
//!
//! * `hs256` -- a shared secret. Local dev and tests; no network, no provider.
//! * `jwks` -- ES256/RS256 public keys fetched (and cached by `kid`) from a
//!   JWKS URL. Every real provider (Supabase included) exposes one.
//!
//! `hybrid` configures both, so a locally minted HS256 token and a
//! provider-issued ES256 token both validate against the same running
//! server.
//!
//! Algorithm confusion is impossible by construction: an alg is only
//! accepted if its matching key material is configured, HS256 is NEVER
//! verified against JWKS keys or vice-versa, and the verification algorithm
//! for an asymmetric token is derived from the JWKS key's own type -- not
//! from the attacker-supplied header.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde_json::Value;
use tokio::sync::{Mutex, RwLock};
use tracing::warn;

use crate::auth::{Claims, TokenVerifier};
use crate::config::{AuthConfig, AuthMode};
use crate::error::AppError;

/// A JWKS key -- RSA (`{n, e}`) or EC (`{x, y}`).
#[derive(Debug, Clone)]
enum JwksKey {
    Rsa { n: String, e: String },
    Ec { x: String, y: String },
}

/// How long an unknown `kid` must wait before it can trigger another JWKS
/// refetch. Desde0 has no such cooldown (checked
/// `/workspace/lula-auth/ref/desde0-backend/src/auth/oidc.rs`); this is a
/// deliberate addition, not a port: without it, anyone sending tokens with
/// garbage `kid`s makes this service hammer the provider's JWKS endpoint
/// once per request.
const JWKS_REFETCH_COOLDOWN: Duration = Duration::from_secs(30);

/// The JWKS half: public keys fetched and cached by `kid`. `fetch_gate`
/// double-duties as a singleflight lock (only the task holding it actually
/// calls the network; everyone else queues behind it and then rechecks the
/// cache) and the refetch cooldown clock.
struct Jwks {
    url: String,
    cache: Arc<RwLock<HashMap<String, JwksKey>>>,
    fetch_gate: Mutex<Option<Instant>>,
    client: reqwest::Client,
}

pub struct OidcVerifier {
    audience: String,
    issuer: String,
    hs256_key: Option<DecodingKey>,
    jwks: Option<Jwks>,
}

impl OidcVerifier {
    /// Build a verifier from config. `hs256`/`jwks` each require their one
    /// field; `hybrid` builds whichever of `jwt_secret`/`jwks_url` is
    /// present and only errors if BOTH are missing. Never called with
    /// `mode = dev`; see `crate::auth::Auth::from_config`.
    pub fn new(config: &AuthConfig) -> anyhow::Result<Self> {
        let non_empty =
            |o: &Option<String>| o.as_deref().filter(|s| !s.is_empty()).map(str::to_string);

        let build_hs256 = || {
            non_empty(&config.jwt_secret).map(|secret| DecodingKey::from_secret(secret.as_bytes()))
        };
        let build_jwks = || -> anyhow::Result<Option<Jwks>> {
            let Some(jwks_url) = non_empty(&config.jwks_url) else {
                return Ok(None);
            };
            // 5s timeout: the auth path holds a handler, so a hung JWKS
            // endpoint must not hang the request.
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()?;
            Ok(Some(Jwks {
                url: jwks_url,
                cache: Arc::new(RwLock::new(HashMap::new())),
                fetch_gate: Mutex::new(None),
                client,
            }))
        };

        let (hs256_key, jwks) = match config.mode {
            AuthMode::Dev => {
                anyhow::bail!("OidcVerifier::new must not be called with auth.mode = dev")
            }
            AuthMode::Hs256 => {
                let key = build_hs256()
                    .ok_or_else(|| anyhow::anyhow!("auth.mode = hs256 requires auth.jwt_secret"))?;
                (Some(key), None)
            }
            AuthMode::Jwks => {
                let jwks = build_jwks()?
                    .ok_or_else(|| anyhow::anyhow!("auth.mode = jwks requires auth.jwks_url"))?;
                (None, Some(jwks))
            }
            AuthMode::Hybrid => {
                let (key, jwks) = (build_hs256(), build_jwks()?);
                if key.is_none() && jwks.is_none() {
                    anyhow::bail!(
                        "auth.mode = hybrid requires auth.jwt_secret and/or auth.jwks_url"
                    );
                }
                (key, jwks)
            }
        };

        Ok(Self {
            audience: config.audience.clone(),
            issuer: config.issuer.clone(),
            hs256_key,
            jwks,
        })
    }

    fn decode_with(
        &self,
        token: &str,
        key: &DecodingKey,
        alg: Algorithm,
    ) -> Result<Claims, AppError> {
        let mut v = Validation::new(alg);
        v.set_audience(&[&self.audience]);
        v.set_issuer(&[&self.issuer]);
        v.validate_exp = true;
        v.validate_nbf = false;
        decode::<Claims>(token, key, &v)
            .map(|d| d.claims)
            .map_err(|e| AppError::Unauthorized(format!("invalid token: validation failed: {e}")))
    }

    async fn validate_jwks(&self, jwks: &Jwks, token: &str) -> Result<Claims, AppError> {
        let header = decode_header(token)
            .map_err(|e| AppError::Unauthorized(format!("invalid token: header: {e}")))?;
        let kid = header
            .kid
            .ok_or_else(|| AppError::Unauthorized("invalid token: missing kid".to_owned()))?;

        // The verification alg comes from the JWKS key's own type, never
        // from the token header: an EC key only ever verifies ES256, an
        // RSA key only RS256.
        let (dk, alg) = match jwks.get_key(&kid).await? {
            JwksKey::Rsa { n, e } => (
                DecodingKey::from_rsa_components(&n, &e)
                    .map_err(|e| AppError::Unauthorized(format!("invalid token: rsa key: {e}")))?,
                Algorithm::RS256,
            ),
            JwksKey::Ec { x, y } => (
                DecodingKey::from_ec_components(&x, &y)
                    .map_err(|e| AppError::Unauthorized(format!("invalid token: ec key: {e}")))?,
                Algorithm::ES256,
            ),
        };
        self.decode_with(token, &dk, alg)
    }
}

#[async_trait]
impl TokenVerifier for OidcVerifier {
    /// Route by the token's declared `alg` to the matching key material,
    /// never across families. Whitelist: HS256 -> shared secret; ES256/
    /// RS256 -> JWKS. Any other alg (including `none`, which
    /// `decode_header` rejects outright) or an alg with no configured key
    /// material is rejected -- no silent pass, no confusion.
    async fn verify(&self, token: &str) -> Result<Claims, AppError> {
        let header = decode_header(token)
            .map_err(|e| AppError::Unauthorized(format!("invalid token: header: {e}")))?;
        match header.alg {
            Algorithm::HS256 => {
                let key = self.hs256_key.as_ref().ok_or_else(|| {
                    AppError::Unauthorized(
                        "invalid token: HS256 token but no shared secret configured".to_owned(),
                    )
                })?;
                self.decode_with(token, key, Algorithm::HS256)
            }
            Algorithm::ES256 | Algorithm::RS256 => {
                let jwks = self.jwks.as_ref().ok_or_else(|| {
                    AppError::Unauthorized(
                        "invalid token: asymmetric token but no JWKS configured".to_owned(),
                    )
                })?;
                self.validate_jwks(jwks, token).await
            }
            other => Err(AppError::Unauthorized(format!(
                "invalid token: unsupported alg: {other:?}"
            ))),
        }
    }
}

impl Jwks {
    async fn get_key(&self, kid: &str) -> Result<JwksKey, AppError> {
        let cached = self.cache.read().await.get(kid).cloned();
        if let Some(k) = cached {
            return Ok(k);
        }

        // Singleflight: only the task that acquires this lock dials the
        // network; every other task queued behind it (whether it is
        // waiting on the same unknown `kid` or a different one) rechecks
        // the cache first, since the fetch it waited for may have already
        // resolved it.
        let mut gate = self.fetch_gate.lock().await;
        let cached = self.cache.read().await.get(kid).cloned();
        if let Some(k) = cached {
            return Ok(k);
        }
        if let Some(last) = *gate
            && last.elapsed() < JWKS_REFETCH_COOLDOWN
        {
            return Err(AppError::Unauthorized(format!(
                "invalid token: kid not found: {kid} (JWKS refetch on cooldown)"
            )));
        }
        // Recorded before the call resolves, successful or not: a
        // malformed/garbage `kid` must not be able to force a refetch on
        // every request regardless of whether the endpoint is up.
        *gate = Some(Instant::now());
        let fetched = fetch_jwks(&self.client, &self.url).await?;
        let mut w = self.cache.write().await;
        w.extend(fetched);
        w.get(kid)
            .cloned()
            .ok_or_else(|| AppError::Unauthorized(format!("invalid token: kid not found: {kid}")))
    }
}

async fn fetch_jwks(
    client: &reqwest::Client,
    url: &str,
) -> Result<HashMap<String, JwksKey>, AppError> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| AppError::ServiceUnavailable(format!("JWKS fetch: {e}")))?
        .error_for_status()
        .map_err(|e| AppError::ServiceUnavailable(format!("JWKS status: {e}")))?;
    let json: Value = resp
        .json()
        .await
        .map_err(|e| AppError::ServiceUnavailable(format!("JWKS parse: {e}")))?;
    // `.get(..)` and not `json["keys"]` throughout: this document came off
    // the network from a provider we do not control, so every field here
    // is untrusted shape. A malformed JWKS response must degrade to an
    // error a caller can see, never abort the task servicing someone's
    // request.
    let keys = json.get("keys").and_then(Value::as_array).ok_or_else(|| {
        AppError::ServiceUnavailable("JWKS response missing keys array".to_owned())
    })?;

    let mut m = HashMap::new();
    for k in keys {
        let field = |name: &str| k.get(name).and_then(Value::as_str);
        let Some(kid) = field("kid") else { continue };
        match field("kty") {
            Some("RSA") => {
                if let (Some(n), Some(e)) = (field("n"), field("e")) {
                    m.insert(
                        kid.into(),
                        JwksKey::Rsa {
                            n: n.into(),
                            e: e.into(),
                        },
                    );
                }
            }
            Some("EC") => {
                if let (Some(x), Some(y)) = (field("x"), field("y")) {
                    m.insert(
                        kid.into(),
                        JwksKey::Ec {
                            x: x.into(),
                            y: y.into(),
                        },
                    );
                }
            }
            other => warn!(kid = %kid, kty = ?other, "JWKS entry with unsupported kty; skipping"),
        }
    }
    Ok(m)
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "a failing assertion aborting the test is the mechanism"
)]
mod tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use serde_json::json;

    const SECRET: &str = "super-secret-jwt-token-with-at-least-32-characters-long";
    const ISS: &str = "http://127.0.0.1:54331/auth/v1";

    fn hs256_config() -> AuthConfig {
        AuthConfig {
            mode: AuthMode::Hs256,
            jwt_secret: Some(SECRET.into()),
            jwks_url: None,
            issuer: ISS.into(),
            audience: "authenticated".into(),
            ..AuthConfig::default()
        }
    }

    fn hybrid_config() -> AuthConfig {
        AuthConfig {
            mode: AuthMode::Hybrid,
            jwt_secret: Some(SECRET.into()),
            jwks_url: Some("http://127.0.0.1:54331/auth/v1/.well-known/jwks.json".into()),
            issuer: ISS.into(),
            audience: "authenticated".into(),
            ..AuthConfig::default()
        }
    }

    /// jwks-only: no HS256 secret, so an HS256 token must be rejected
    /// outright rather than verified against the JWKS public keys. The URL
    /// is never reached (no asymmetric token is exercised here), so no
    /// network call happens.
    fn jwks_only_config() -> AuthConfig {
        AuthConfig {
            mode: AuthMode::Jwks,
            jwt_secret: None,
            jwks_url: Some("http://127.0.0.1:1/unused".into()),
            issuer: ISS.into(),
            audience: "authenticated".into(),
            ..AuthConfig::default()
        }
    }

    fn mint(secret: &str, claims: &serde_json::Value) -> String {
        encode(
            &Header::new(Algorithm::HS256),
            claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .expect("encode")
    }

    fn base_claims(exp_offset: i64) -> serde_json::Value {
        json!({
            "sub": "11111111-2222-3333-4444-555555555555",
            "aud": "authenticated",
            "iss": ISS,
            "exp": chrono::Utc::now().timestamp() + exp_offset,
        })
    }

    #[tokio::test]
    async fn hs256_valid_token_passes() {
        let v = OidcVerifier::new(&hs256_config()).unwrap();
        let claims = v.verify(&mint(SECRET, &base_claims(3600))).await.unwrap();
        assert_eq!(claims.sub, "11111111-2222-3333-4444-555555555555");
        assert_eq!(claims.aud, "authenticated");
    }

    #[tokio::test]
    async fn missing_email_and_aal_still_decode() {
        let v = OidcVerifier::new(&hs256_config()).unwrap();
        let claims = v.verify(&mint(SECRET, &base_claims(3600))).await.unwrap();
        assert!(claims.email.is_none());
        assert!(claims.aal.is_none());
    }

    #[tokio::test]
    async fn aal_and_app_metadata_round_trip() {
        let v = OidcVerifier::new(&hs256_config()).unwrap();
        let mut c = base_claims(3600);
        c["aal"] = json!("aal2");
        c["app_metadata"] = json!({"role": "admin"});
        let claims = v.verify(&mint(SECRET, &c)).await.unwrap();
        assert_eq!(claims.aal.as_deref(), Some("aal2"));
        assert_eq!(claims.app_metadata.unwrap()["role"], "admin");
    }

    #[tokio::test]
    async fn wrong_issuer_rejected() {
        let v = OidcVerifier::new(&hs256_config()).unwrap();
        let mut c = base_claims(3600);
        c["iss"] = json!("https://evil.example.com");
        assert!(v.verify(&mint(SECRET, &c)).await.is_err());
    }

    #[tokio::test]
    async fn expired_rejected() {
        let v = OidcVerifier::new(&hs256_config()).unwrap();
        assert!(v.verify(&mint(SECRET, &base_claims(-3600))).await.is_err());
    }

    #[tokio::test]
    async fn wrong_secret_rejected() {
        let v = OidcVerifier::new(&hs256_config()).unwrap();
        let token = mint(
            "different-secret-thats-also-32-characters-long!",
            &base_claims(3600),
        );
        assert!(v.verify(&token).await.is_err());
    }

    #[test]
    fn hs256_requires_secret() {
        let cfg = AuthConfig {
            mode: AuthMode::Hs256,
            jwt_secret: None,
            ..hs256_config()
        };
        assert!(OidcVerifier::new(&cfg).is_err());
    }

    #[test]
    fn dev_mode_refuses_to_construct_a_verifier() {
        let cfg = AuthConfig {
            mode: AuthMode::Dev,
            ..hs256_config()
        };
        assert!(OidcVerifier::new(&cfg).is_err());
    }

    // ── hybrid / alg routing (security-critical) ───────────────────

    #[tokio::test]
    async fn hybrid_still_validates_hs256() {
        let v = OidcVerifier::new(&hybrid_config()).unwrap();
        let claims = v.verify(&mint(SECRET, &base_claims(3600))).await.unwrap();
        assert_eq!(claims.sub, "11111111-2222-3333-4444-555555555555");
    }

    #[tokio::test]
    async fn hybrid_rejects_hs256_signed_with_wrong_secret() {
        // Confusion guard: the JWKS half must not rescue a bad HS256
        // signature.
        let v = OidcVerifier::new(&hybrid_config()).unwrap();
        let token = mint(
            "different-secret-thats-also-32-characters-long!",
            &base_claims(3600),
        );
        assert!(v.verify(&token).await.is_err());
    }

    #[tokio::test]
    async fn hs256_token_rejected_when_no_secret_configured() {
        let v = OidcVerifier::new(&jwks_only_config()).unwrap();
        assert!(v.verify(&mint(SECRET, &base_claims(3600))).await.is_err());
    }

    #[tokio::test]
    async fn alg_none_rejected() {
        // Unsigned `alg: none`. `decode_header` parses only the header,
        // which has no `none` variant in `Algorithm`, so it errors before
        // the payload matters.
        let token = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJzdWIiOiJ4In0.";
        let v = OidcVerifier::new(&hybrid_config()).unwrap();
        assert!(v.verify(token).await.is_err());
    }

    #[tokio::test]
    async fn unknown_alg_rejected() {
        // HS384 is a real alg but not whitelisted, so it is rejected, not
        // verified.
        let token = encode(
            &Header::new(Algorithm::HS384),
            &base_claims(3600),
            &EncodingKey::from_secret(SECRET.as_bytes()),
        )
        .expect("encode");
        let v = OidcVerifier::new(&hybrid_config()).unwrap();
        assert!(v.verify(&token).await.is_err());
    }

    #[test]
    fn hybrid_requires_at_least_one_key() {
        let cfg = AuthConfig {
            mode: AuthMode::Hybrid,
            jwt_secret: None,
            jwks_url: None,
            ..hybrid_config()
        };
        assert!(OidcVerifier::new(&cfg).is_err());
    }
}
