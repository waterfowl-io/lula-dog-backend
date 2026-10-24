//! Supabase JWT verification and the `AuthUser` extractor.
//!
//! `AUTH_MODE=supabase` (the default) verifies a bearer JWT: ES256/RS256
//! against the project's JWKS (`${SUPABASE_URL}/auth/v1/.well-known/jwks.json`,
//! cached with a TTL and refetched on an unknown `kid`), or HS256 against the
//! legacy `SUPABASE_JWT_SECRET` for projects still on that scheme. `exp`,
//! `aud` and `iss` are all validated.
//!
//! `AUTH_MODE=dev` bypasses verification entirely: every request is a fixed
//! dev user, overridable per-request with `X-Dev-User: <uuid>` (handy for
//! exercising workspace-membership checks against several "users" without
//! real tokens). It only activates on an explicit `AUTH_MODE=dev`, never as
//! a fallback, and logs a loud warning on startup.

pub mod jwks;

use std::time::Duration;

use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use uuid::Uuid;

use crate::config;
use crate::error::{AppError, AppResult};
use jwks::Jwks;

/// The authenticated caller: a Supabase Auth user id and email, or their dev
/// equivalents under `AUTH_MODE=dev`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthUser {
    pub id: Uuid,
    pub email: String,
}

pub enum Auth {
    Dev {
        default_user_id: Uuid,
    },
    Supabase {
        /// `${SUPABASE_URL}/auth/v1`.
        iss: String,
        aud: String,
        /// Legacy HS256 fallback; `None` means an HS256 token is always rejected.
        jwt_secret: Option<String>,
        jwks: Box<Jwks>,
    },
}

impl Auth {
    pub fn from_config(mode: &config::AuthMode) -> anyhow::Result<Self> {
        Ok(match mode {
            config::AuthMode::Dev { default_user_id } => {
                tracing::warn!(
                    %default_user_id,
                    "AUTH_MODE=dev: JWT verification is DISABLED. Every request is treated as \
                     the user above unless overridden with the X-Dev-User header. Do not run \
                     this in production."
                );
                Self::Dev {
                    default_user_id: *default_user_id,
                }
            }
            config::AuthMode::Supabase {
                supabase_url,
                aud,
                jwt_secret,
                jwks_ttl_secs,
            } => {
                let base = supabase_url.trim_end_matches('/');
                Self::Supabase {
                    iss: format!("{base}/auth/v1"),
                    aud: aud.clone(),
                    jwt_secret: jwt_secret.clone(),
                    jwks: Box::new(Jwks::new(
                        format!("{base}/auth/v1/.well-known/jwks.json"),
                        Duration::from_secs(*jwks_ttl_secs),
                    )?),
                }
            }
        })
    }

    pub async fn authenticate(&self, parts: &Parts) -> AppResult<AuthUser> {
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
                Ok(AuthUser {
                    id,
                    email: format!("dev+{id}@local.test"),
                })
            }
            Self::Supabase {
                iss,
                aud,
                jwt_secret,
                jwks,
            } => {
                let header = parts.headers.get(AUTHORIZATION).ok_or_else(|| {
                    AppError::Unauthorized("missing Authorization header".to_owned())
                })?;
                let value = header.to_str().map_err(|_| {
                    AppError::Unauthorized("Authorization header is not valid UTF-8".to_owned())
                })?;
                let token = value.strip_prefix("Bearer ").ok_or_else(|| {
                    AppError::Unauthorized(
                        "Authorization header must be `Bearer <token>`".to_owned(),
                    )
                })?;
                let claims = verify(token, iss, aud, jwt_secret.as_deref(), jwks).await?;
                let id = Uuid::parse_str(&claims.sub)
                    .map_err(|_| AppError::Unauthorized("token `sub` is not a uuid".to_owned()))?;
                let email = claims.email.ok_or_else(|| {
                    AppError::Unauthorized("token has no `email` claim".to_owned())
                })?;
                Ok(AuthUser { id, email })
            }
        }
    }
}

impl<S> FromRequestParts<S> for AuthUser
where
    S: AsRef<Auth> + Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        state.as_ref().authenticate(parts).await
    }
}

#[derive(Debug, Deserialize)]
struct Claims {
    sub: String,
    email: Option<String>,
}

/// Verify `token`, dispatching on its header `alg`: HS256 against
/// `jwt_secret` (the legacy Supabase scheme), ES256/RS256 against the key
/// named by the header's `kid` in `jwks`. `exp`, `aud` and `iss` are
/// validated against the token's claims by `jsonwebtoken` itself.
async fn verify(
    token: &str,
    iss: &str,
    aud: &str,
    jwt_secret: Option<&str>,
    jwks: &Jwks,
) -> AppResult<Claims> {
    let header = jsonwebtoken::decode_header(token)
        .map_err(|e| AppError::Unauthorized(format!("malformed JWT header: {e}")))?;
    let (algorithm, decoding_key) = match header.alg {
        Algorithm::HS256 => {
            let secret = jwt_secret.ok_or_else(|| {
                AppError::Unauthorized(
                    "HS256 token received but SUPABASE_JWT_SECRET is not configured".to_owned(),
                )
            })?;
            (
                Algorithm::HS256,
                DecodingKey::from_secret(secret.as_bytes()),
            )
        }
        alg @ (Algorithm::ES256 | Algorithm::RS256) => {
            let kid = header
                .kid
                .ok_or_else(|| AppError::Unauthorized("JWT header missing `kid`".to_owned()))?;
            let key = jwks
                .key_for(&kid)
                .await
                .map_err(|e| AppError::Unauthorized(e.to_string()))?;
            if key.algorithm != alg {
                return Err(AppError::Unauthorized(
                    "JWT `alg` does not match the JWKS key's algorithm".to_owned(),
                ));
            }
            (key.algorithm, key.decoding_key)
        }
        other => {
            return Err(AppError::Unauthorized(format!(
                "unsupported JWT algorithm {other:?}"
            )));
        }
    };
    let mut validation = Validation::new(algorithm);
    validation.set_audience(&[aud]);
    validation.set_issuer(&[iss]);
    let data = jsonwebtoken::decode::<Claims>(token, &decoding_key, &validation)
        .map_err(|e| AppError::Unauthorized(format!("invalid token: {e}")))?;
    Ok(data.claims)
}
