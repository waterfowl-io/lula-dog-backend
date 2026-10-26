//! The one error type handlers return. Every error goes out as
//! `{"error": {"code": "...", "message": "..."}}`.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0} not found")]
    NotFound(&'static str),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Unauthorized(String),
    #[error("{0}")]
    Forbidden(String),
    /// Below `aal2` on a route that requires it (every non-public route
    /// except `GET /me`; see `crate::auth`). A distinct code from
    /// `Forbidden` so the frontend can route straight to the TOTP
    /// challenge instead of showing a generic access-denied state.
    #[error("multi-factor authentication required")]
    MfaRequired,
    /// The identity provider's JWKS endpoint is unreachable or erroring.
    /// Distinct from `Unauthorized`: the token may well be valid, so a
    /// caller should retry rather than re-authenticate.
    #[error("{0}")]
    ServiceUnavailable(String),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    /// A server-side invariant was violated (never caused by the request).
    /// The detail is logged, never returned to the caller.
    #[error("{0}")]
    Internal(String),
    /// A real, planned capability that isn't built yet -- e.g. build-mode
    /// chat in phase 1 (see `crate::chat::handlers::stream_chat`). Distinct
    /// from `Internal`: the message is safe to return to the caller.
    #[error("{0}")]
    NotImplemented(String),
}

pub type AppResult<T> = Result<T, AppError>;

// Extractor rejections (malformed JSON, unknown fields, a non-numeric id) come
// back in the same JSON shape as every other error. See `crate::extract`.
impl From<axum::extract::rejection::JsonRejection> for AppError {
    fn from(r: axum::extract::rejection::JsonRejection) -> Self {
        Self::BadRequest(r.body_text())
    }
}

impl From<axum::extract::rejection::PathRejection> for AppError {
    fn from(r: axum::extract::rejection::PathRejection) -> Self {
        Self::BadRequest(r.body_text())
    }
}

impl From<axum::extract::rejection::QueryRejection> for AppError {
    fn from(r: axum::extract::rejection::QueryRejection) -> Self {
        Self::BadRequest(r.body_text())
    }
}

#[derive(Serialize, ToSchema)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Serialize, ToSchema)]
pub struct ErrorDetail {
    /// Stable machine-readable code: bad_request, not_found, conflict, internal.
    pub code: &'static str,
    pub message: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code, message) = match &self {
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, "bad_request", m.clone()),
            Self::NotFound(_) => (StatusCode::NOT_FOUND, "not_found", self.to_string()),
            Self::Conflict(m) => (StatusCode::CONFLICT, "conflict", m.clone()),
            Self::Unauthorized(m) => (StatusCode::UNAUTHORIZED, "unauthorized", m.clone()),
            Self::Forbidden(m) => (StatusCode::FORBIDDEN, "forbidden", m.clone()),
            Self::MfaRequired => (StatusCode::FORBIDDEN, "mfa_required", self.to_string()),
            Self::ServiceUnavailable(m) => {
                tracing::warn!(error = %m, "identity provider unavailable");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "service_unavailable",
                    "auth temporarily unavailable".to_owned(),
                )
            }
            Self::Database(e) => {
                tracing::error!(error = %e, "database error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "internal error".to_owned(),
                )
            }
            Self::Internal(m) => {
                tracing::error!(error = %m, "internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "internal error".to_owned(),
                )
            }
            Self::NotImplemented(m) => (StatusCode::NOT_IMPLEMENTED, "not_implemented", m.clone()),
        };
        (
            status,
            Json(ErrorBody {
                error: ErrorDetail { code, message },
            }),
        )
            .into_response()
    }
}
