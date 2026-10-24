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
    #[error(transparent)]
    Database(#[from] sqlx::Error),
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
            Self::Database(e) => {
                tracing::error!(error = %e, "database error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "internal error".to_owned(),
                )
            }
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
