//! Bearer-token verification, the `AuthUser` extractor, and profile/invite
//! provisioning. Ported from Desde0's `TokenVerifier`/`OidcVerifier` seam
//! (`crate::auth::oidc`), which knows nothing about which identity provider
//! issued the token -- it is configured entirely from `[auth]` (issuer,
//! audience, key material).
//!
//! `auth.mode = "jwks"` (the default) verifies against the project's JWKS;
//! `"hs256"` against a shared secret; `"hybrid"` both, dispatching per
//! token by its header `alg` -- see `oidc::OidcVerifier`. `auth.mode =
//! "dev"` bypasses verification entirely: every request is a fixed dev
//! user, overridable per-request with `X-Dev-User: <uuid>`. It only
//! activates on that explicit mode (never a fallback), logs a loud warning
//! on startup, and `Config::validate` refuses to boot in it outside
//! `Env::Dev`.
//!
//! Every route the router registers requires `aal2` (a second factor on
//! THIS session) except `GET /me`, so the frontend can render the TOTP
//! enrollment/challenge step before anything else. `AUTH_MODE=dev`/
//! `auth.mode = dev` is always treated as `aal2`. Below it: 403
//! `mfa_required` (`AppError::MfaRequired`), not the generic `forbidden` a
//! membership check returns, so the frontend can route straight to the
//! challenge screen. Routes that take no `AuthUser` at all (`/health`,
//! `/openapi.json`, `/app-definition.schema.json` -- see [`is_public`]) are
//! never gated, by construction: nothing in them calls the extractor.
//!
//! Provisioning (`provision`, best-effort with a 500ms ceiling) upserts the
//! caller's `profiles` row and claims any pending `workspace_invites` for
//! their verified email -- see `crate::control::invites`.

pub mod oidc;

use std::time::Duration;

use async_trait::async_trait;
use axum::extract::FromRequestParts;
use axum::http::Method;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::AppState;
use crate::config::{self, AuthMode};
use crate::control::ControlPool;
use crate::error::{AppError, AppResult};

/// Paths served with no `AuthUser` extraction at all. Purely descriptive --
/// nothing in this module consults it to let a request through, since a
/// public route simply never names `AuthUser` as a handler parameter. Kept
/// as one named list so `tests/auth.rs`'s router-walk test can state, in
/// one place, which routes it expects to be exempt.
pub fn is_public(path: &str) -> bool {
    matches!(
        path,
        "/health" | "/openapi.json" | "/app-definition.schema.json"
    )
}

/// The claims this service relies on. `sub` is the stable identity;
/// `email` is required downstream (checked in the extractor, not here,
/// because invite acceptance needs it and a serde-level rejection would
/// give a worse error than `AppError::Unauthorized`). `aal`/`app_metadata`
/// are optional because most providers and most tokens carry nothing in
/// them; absent `aal` reads as below `aal2` everywhere it is judged, so the
/// gate fails closed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    #[serde(deserialize_with = "deserialize_aud")]
    pub aud: String,
    pub iss: String,
    pub exp: i64,
    #[serde(default)]
    pub iat: Option<i64>,
    #[serde(default)]
    pub email: Option<String>,
    /// Supabase-style claim: `Some(false)` means the provider has not
    /// confirmed this address. Absent (most providers, and every token
    /// `oidc::tests` mints) is treated as verified, since Desde0's
    /// `Claims` carries no such claim at all and this service has no
    /// stronger signal to fall back on; see `provision`.
    #[serde(default)]
    pub email_verified: Option<bool>,
    /// Authenticator assurance level ("aal1" after a password alone,
    /// "aal2" after a second factor).
    #[serde(default)]
    pub aal: Option<String>,
    /// Server-set application metadata (role claims live here in Desde0;
    /// unused by Lula today, carried for parity and future use).
    #[serde(default)]
    pub app_metadata: Option<Value>,
}

/// Deserialize `aud`, which OIDC providers emit as either a string or an
/// array. Take the first string in either case.
fn deserialize_aud<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    use serde::de::Error;
    let v: Value = serde::Deserialize::deserialize(d)?;
    match v {
        Value::String(s) => Ok(s),
        Value::Array(arr) => arr
            .first()
            .and_then(|x| x.as_str())
            .map(String::from)
            .ok_or_else(|| D::Error::custom("invalid aud")),
        _ => Err(D::Error::custom("invalid aud")),
    }
}

/// The authenticated caller: a verified user id and email (or their dev
/// equivalents under `auth.mode = "dev"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthUser {
    pub id: Uuid,
    pub email: String,
}

/// The auth seam: turn a bearer token into claims, or reject it. One
/// method, no provider vocabulary -- everything provider-shaped (JWKS
/// fetching, algorithm routing, key caching) lives behind an
/// implementation (`oidc::OidcVerifier` is the real one).
#[async_trait]
pub trait TokenVerifier: Send + Sync {
    async fn verify(&self, token: &str) -> Result<Claims, AppError>;
}

pub enum Auth {
    Dev { default_user_id: Uuid },
    Oidc(Box<dyn TokenVerifier>),
}

impl Auth {
    pub fn from_config(mode: &config::AuthConfig) -> anyhow::Result<Self> {
        Ok(match mode.mode {
            AuthMode::Dev => {
                tracing::warn!(
                    default_user_id = %mode.dev_user_id,
                    "auth.mode = dev: JWT verification is DISABLED. Every request is treated \
                     as the user above (at aal2) unless overridden with the X-Dev-User header. \
                     Do not run this in production."
                );
                Self::Dev {
                    default_user_id: mode.dev_user_id,
                }
            }
            _ => Self::Oidc(Box::new(oidc::OidcVerifier::new(mode)?)),
        })
    }

    /// Verify the request (or synthesize dev claims) with no further
    /// judgment -- `sub`/`email` parsing, the `aal2` gate and provisioning
    /// all happen in `AuthUser::from_request_parts`, the one call site.
    async fn claims(&self, parts: &Parts) -> AppResult<Claims> {
        match self {
            Self::Dev { default_user_id } => {
                let id = match parts.headers.get("x-dev-user") {
                    None => *default_user_id,
                    Some(value) => {
                        let value = value.to_str().map_err(|_| {
                            AppError::BadRequest("X-Dev-User is not valid UTF-8".to_owned())
                        })?;
                        Uuid::parse_str(value).map_err(|_| {
                            AppError::BadRequest("X-Dev-User is not a uuid".to_owned())
                        })?
                    }
                };
                Ok(Claims {
                    sub: id.to_string(),
                    aud: "authenticated".to_owned(),
                    iss: "dev".to_owned(),
                    exp: i64::MAX,
                    iat: None,
                    email: Some(format!("dev+{id}@local.test")),
                    email_verified: Some(true),
                    // Dev mode is always treated as aal2 -- there is no
                    // second factor to challenge against a bypass.
                    aal: Some("aal2".to_owned()),
                    app_metadata: None,
                })
            }
            Self::Oidc(verifier) => {
                let header = parts
                    .headers
                    .get(AUTHORIZATION)
                    .ok_or(AppError::MissingToken)?;
                let value = header.to_str().map_err(|_| {
                    AppError::Unauthorized("Authorization header is not valid UTF-8".to_owned())
                })?;
                let token = value.strip_prefix("Bearer ").ok_or_else(|| {
                    AppError::Unauthorized(
                        "Authorization header must be `Bearer <token>`".to_owned(),
                    )
                })?;
                verifier.verify(token).await
            }
        }
    }
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, AppError> {
        let claims = state.auth.claims(parts).await?;
        let id = Uuid::parse_str(&claims.sub)
            .map_err(|_| AppError::Unauthorized("token `sub` is not a uuid".to_owned()))?;
        let email = claims
            .email
            .clone()
            .ok_or_else(|| AppError::Unauthorized("token has no `email` claim".to_owned()))?;

        // Every route is gated except `GET /me`, so the frontend can learn
        // it needs to enroll/challenge TOTP before anything else renders.
        let is_me = parts.method == Method::GET && parts.uri.path() == "/me";
        if !is_me && claims.aal.as_deref() != Some("aal2") {
            return Err(AppError::MfaRequired);
        }

        if let Some(control) = &state.control {
            provision(control, id, &email, claims.email_verified).await;
        }

        Ok(Self { id, email })
    }
}

/// Upsert the caller's `profiles` row and claim any pending
/// `workspace_invites` for their email. Best-effort, with a 500ms ceiling
/// so a wedged connection cannot stall every request: the provider has
/// already authenticated this user, and a transient DB hiccup must not
/// reject them. A persistent outage still surfaces -- the handler's own DB
/// call fails.
async fn provision(control: &ControlPool, id: Uuid, email: &str, email_verified: Option<bool>) {
    let fut = async {
        crate::control::profiles::upsert_from_jwt(control, id, email).await?;
        // `Some(false)` is the only signal that distinguishes an
        // unconfirmed address from everything else (most providers, and
        // every dev/test token, carry no claim at all); invite acceptance
        // is keyed on this email, so an explicitly-unconfirmed one must
        // not claim someone else's pending invite.
        if email_verified != Some(false) {
            crate::control::invites::accept_pending_for_email(control, id, email).await?;
        }
        Ok::<(), AppError>(())
    };
    match tokio::time::timeout(Duration::from_millis(500), fut).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, "profile/invite provisioning failed"),
        Err(_) => tracing::warn!("profile/invite provisioning timed out"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_three_unauthenticated_routes_are_public() {
        assert!(is_public("/health"));
        assert!(is_public("/openapi.json"));
        assert!(is_public("/app-definition.schema.json"));
        assert!(!is_public("/me"));
        assert!(!is_public("/workspaces"));
        assert!(!is_public(
            "/w/00000000-0000-0000-0000-000000000000/opportunities"
        ));
    }
}
