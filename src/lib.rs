//! Lula.dog backend: structured sales data for AI agents. SQLite is the
//! system of record.

pub mod config;
pub mod db;
pub mod error;
pub mod extract;
pub mod time;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use sqlx::SqlitePool;
use utoipa::{OpenApi, ToSchema};

use crate::error::AppResult;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/openapi.json", get(openapi))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}

#[derive(Serialize, ToSchema)]
pub struct Health {
    pub status: &'static str,
}

/// Liveness plus a database round trip.
#[utoipa::path(
    get,
    path = "/health",
    responses((status = 200, body = Health), (status = 500, body = error::ErrorBody)),
    tag = "health"
)]
pub async fn health(State(state): State<AppState>) -> AppResult<Json<Health>> {
    sqlx::query("SELECT 1").execute(&state.pool).await?;
    Ok(Json(Health { status: "ok" }))
}

#[derive(OpenApi)]
#[openapi(
    info(title = "Lula.dog API", description = "Sales opportunity tracker"),
    paths(health)
)]
pub struct ApiDoc;

/// The OpenAPI document for this API.
async fn openapi() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}
