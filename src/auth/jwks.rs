//! JWKS fetch and cache for `AuthMode::Supabase`. Keys are fetched from
//! `${SUPABASE_URL}/auth/v1/.well-known/jwks.json`, cached for a TTL, and
//! refetched early if a token names a `kid` the cache doesn't have (covers
//! key rotation without waiting out the TTL).

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, bail};
use jsonwebtoken::{Algorithm, DecodingKey};
use serde::Deserialize;
use tokio::sync::RwLock;
use tokio::time::Instant;

#[derive(Debug, Deserialize)]
struct JwkSet {
    keys: Vec<Jwk>,
}

#[derive(Debug, Deserialize)]
struct Jwk {
    kty: String,
    kid: Option<String>,
    crv: Option<String>,
    n: Option<String>,
    e: Option<String>,
    x: Option<String>,
    y: Option<String>,
}

/// A decoding key plus the algorithm it is valid for, so a token can't claim
/// a `kid` was issued for one algorithm and verify it as another.
#[derive(Clone)]
pub struct JwksKey {
    pub algorithm: Algorithm,
    pub decoding_key: DecodingKey,
}

fn parse_jwk(jwk: &Jwk) -> anyhow::Result<(Algorithm, DecodingKey)> {
    match jwk.kty.as_str() {
        "RSA" => {
            let n = jwk.n.as_deref().context("RSA JWK missing `n`")?;
            let e = jwk.e.as_deref().context("RSA JWK missing `e`")?;
            Ok((Algorithm::RS256, DecodingKey::from_rsa_components(n, e)?))
        }
        "EC" => {
            let crv = jwk.crv.as_deref().context("EC JWK missing `crv`")?;
            if crv != "P-256" {
                bail!("unsupported EC curve `{crv}`, only P-256 (ES256) is supported");
            }
            let x = jwk.x.as_deref().context("EC JWK missing `x`")?;
            let y = jwk.y.as_deref().context("EC JWK missing `y`")?;
            Ok((Algorithm::ES256, DecodingKey::from_ec_components(x, y)?))
        }
        other => bail!("unsupported JWK key type `{other}`"),
    }
}

struct Cache {
    fetched_at: Instant,
    keys: HashMap<String, JwksKey>,
}

pub struct Jwks {
    url: String,
    ttl: Duration,
    client: reqwest::Client,
    cache: RwLock<Option<Cache>>,
}

impl Jwks {
    pub fn new(url: String, ttl: Duration) -> anyhow::Result<Self> {
        Ok(Self {
            url,
            ttl,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()?,
            cache: RwLock::new(None),
        })
    }

    async fn fetch(&self) -> anyhow::Result<HashMap<String, JwksKey>> {
        let set: JwkSet = self
            .client
            .get(&self.url)
            .send()
            .await
            .context("fetching JWKS")?
            .error_for_status()
            .context("JWKS endpoint returned an error status")?
            .json()
            .await
            .context("parsing JWKS response")?;
        let mut keys = HashMap::with_capacity(set.keys.len());
        for jwk in &set.keys {
            let Some(kid) = jwk.kid.clone() else { continue };
            match parse_jwk(jwk) {
                Ok((algorithm, decoding_key)) => {
                    keys.insert(
                        kid,
                        JwksKey {
                            algorithm,
                            decoding_key,
                        },
                    );
                }
                Err(e) => tracing::warn!(kid, error = %e, "skipping unsupported JWKS key"),
            }
        }
        Ok(keys)
    }

    /// The key for `kid`, refetching if the cache is stale or doesn't have
    /// it (once per call: a `kid` that is still missing after a refetch is
    /// genuinely unknown, not just cold-cache).
    pub async fn key_for(&self, kid: &str) -> anyhow::Result<JwksKey> {
        {
            let guard = self.cache.read().await;
            if let Some(cache) = guard.as_ref()
                && cache.fetched_at.elapsed() < self.ttl
                && let Some(key) = cache.keys.get(kid)
            {
                return Ok(key.clone());
            }
        }
        let keys = self.fetch().await?;
        let key = keys
            .get(kid)
            .cloned()
            .with_context(|| format!("no JWKS key found for kid `{kid}`"))?;
        *self.cache.write().await = Some(Cache {
            fetched_at: Instant::now(),
            keys,
        });
        Ok(key)
    }
}
