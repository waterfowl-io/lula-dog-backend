//! Lula.dog backend: structured sales data for AI agents. SQLite is the
//! system of record.

pub mod alerts;
pub mod bucket;
pub mod classifier;
pub mod companies;
pub mod config;
pub mod coverage;
pub mod crawler;
pub mod db;
pub mod enrichment;
pub mod error;
pub mod extract;
pub mod extractor;
pub mod importers;
pub mod leads;
pub mod opportunities;
pub mod paste;
pub mod places;
pub mod proposals;
pub mod seed;
pub mod target_profiles;
pub mod tasks;
pub mod time;
pub mod timeline;

use std::sync::Arc;

use axum::extract::State;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::Serialize;
use sqlx::SqlitePool;
use utoipa::{OpenApi, ToSchema};

use crate::classifier::{Classifier, GrokClassifier, StubClassifier};
use crate::config::{ClassifierConfig, CrawlerConfig, ExtractorConfig, PlacesConfig};
use crate::crawler::{Fetcher, HttpFetcher, StubFetcher};
use crate::error::AppResult;
use crate::extractor::{Extractor, GrokExtractor, StubExtractor};
use crate::places::{GooglePlacesClient, PlacesClient, StubPlacesClient};

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub extractor: Arc<dyn Extractor>,
}

pub fn extractor_from_config(config: &ExtractorConfig) -> anyhow::Result<Arc<dyn Extractor>> {
    Ok(match config {
        ExtractorConfig::Stub => Arc::new(StubExtractor),
        ExtractorConfig::Grok {
            api_key,
            model,
            base_url,
        } => Arc::new(GrokExtractor::new(
            api_key.clone(),
            model.clone(),
            base_url,
        )?),
    })
}

pub fn places_from_config(config: &PlacesConfig) -> anyhow::Result<Arc<dyn PlacesClient>> {
    Ok(match config {
        PlacesConfig::Stub => Arc::new(StubPlacesClient),
        PlacesConfig::Google { api_key } => Arc::new(GooglePlacesClient::new(
            api_key.clone(),
            "https://places.googleapis.com/v1",
        )?),
    })
}

pub fn fetcher_from_config(
    config: CrawlerConfig,
    limits: &config::CrawlLimits,
) -> anyhow::Result<Arc<dyn Fetcher>> {
    Ok(match config {
        CrawlerConfig::Stub => Arc::new(StubFetcher),
        CrawlerConfig::Http => Arc::new(HttpFetcher::new(
            std::time::Duration::from_secs(limits.timeout_secs),
            limits.max_bytes,
        )?),
    })
}

pub fn classifier_from_config(config: &ClassifierConfig) -> anyhow::Result<Arc<dyn Classifier>> {
    Ok(match config {
        ClassifierConfig::Stub => Arc::new(StubClassifier),
        ClassifierConfig::Grok {
            api_key,
            model,
            base_url,
        } => Arc::new(GrokClassifier::new(
            api_key.clone(),
            model.clone(),
            base_url,
        )?),
    })
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/openapi.json", get(openapi))
        .route(
            "/opportunities",
            get(opportunities::list).post(opportunities::create),
        )
        .route(
            "/opportunities/{id}",
            get(opportunities::get).patch(opportunities::patch),
        )
        .route("/opportunities/{id}/tasks", post(tasks::create))
        .route("/opportunities/{id}/paste", post(paste::paste))
        .route(
            "/opportunities/{id}/proposals/accept-all",
            post(proposals::accept_all),
        )
        .route(
            "/opportunities/{id}/proposals/dismiss-all",
            post(proposals::dismiss_all),
        )
        .route("/tasks/{id}", patch(tasks::patch).delete(tasks::delete))
        .route("/proposals/{id}/accept", post(proposals::accept))
        .route("/proposals/{id}/reject", post(proposals::reject))
        .route("/alerts/stale", get(alerts::stale))
        .route(
            "/target-profiles",
            get(target_profiles::list_handler).post(target_profiles::create),
        )
        .route(
            "/target-profiles/{id}",
            get(target_profiles::get)
                .patch(target_profiles::patch)
                .delete(target_profiles::delete),
        )
        .route("/leads", get(leads::list))
        .route("/leads/{id}/accept", post(leads::accept))
        .route("/leads/{id}/reject", post(leads::reject))
        .route("/leads/{id}/snooze", post(leads::snooze))
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
    paths(
        health,
        opportunities::list,
        opportunities::create,
        opportunities::get,
        opportunities::patch,
        tasks::create,
        tasks::patch,
        tasks::delete,
        paste::paste,
        proposals::accept,
        proposals::reject,
        proposals::accept_all,
        proposals::dismiss_all,
        alerts::stale,
        target_profiles::list_handler,
        target_profiles::create,
        target_profiles::get,
        target_profiles::patch,
        target_profiles::delete,
        leads::list,
        leads::accept,
        leads::reject,
        leads::snooze,
    )
)]
pub struct ApiDoc;

/// The OpenAPI document for this API.
async fn openapi() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}
