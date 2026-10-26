//! Lula.dog backend: structured sales data for AI agents. Supabase Postgres
//! is the control plane (workspaces, membership, app definitions, saved
//! queries); one SQLite file per workspace is the system of record for that
//! workspace's editable data. See `docs/architecture.md`.

pub mod actions;
pub mod alerts;
pub mod app_definition;
pub mod apps;
pub mod auth;
pub mod bucket;
pub mod chat;
pub mod classifier;
pub mod companies;
pub mod config;
pub mod config_loader;
pub mod control;
pub mod coverage;
pub mod crawler;
pub mod db;
pub mod enrichment;
pub mod error;
pub mod extract;
pub mod extractor;
pub mod importers;
pub mod leads;
pub mod me;
pub mod opportunities;
pub mod paste;
pub mod places;
pub mod proposals;
pub mod queries;
pub mod query_runner;
pub mod schema;
pub mod seed;
pub mod target_profiles;
pub mod tasks;
pub mod time;
pub mod timeline;
pub mod workspace_db;

use std::sync::Arc;

use axum::extract::State;
use axum::routing::{get, patch, post};
use axum::{Json, Router, middleware};
use serde::Serialize;
use utoipa::{OpenApi, ToSchema};

use crate::auth::Auth;
use crate::chat::build::BuildAgent;
use crate::chat::provider::ChatProvider;
use crate::classifier::{Classifier, GrokClassifier, StubClassifier};
use crate::config::{ChatConfig, ClassifierConfig, CrawlerConfig, ExtractorConfig, PlacesConfig};
use crate::control::ControlPool;
use crate::crawler::{Fetcher, HttpFetcher, StubFetcher};
use crate::error::AppResult;
use crate::extractor::{Extractor, GrokExtractor, StubExtractor};
use crate::places::{GooglePlacesClient, PlacesClient, StubPlacesClient};
use crate::workspace_db::WorkspaceRegistry;

#[derive(Clone)]
pub struct AppState {
    pub extractor: Arc<dyn Extractor>,
    /// The Postgres control plane. `None` disables workspace membership
    /// enforcement (every workspace id is accessible as `owner`) -- see
    /// `workspace_db::scope`.
    pub control: Option<ControlPool>,
    pub workspaces: WorkspaceRegistry,
    pub auth: Arc<Auth>,
    /// Hard cap on rows returned by `POST /w/{ws}/queries/{id}/run`.
    pub query_max_rows: i64,
    pub query_timeout_ms: u64,
    pub chat: ChatConfig,
    pub chat_provider: Arc<dyn ChatProvider>,
    /// Build-mode chat's execution seam -- see `crate::chat::build`'s module
    /// doc comment for why a whole turn, not just the LLM call, sits behind
    /// this trait.
    pub build_agent: Arc<dyn BuildAgent>,
    /// The CORS allowlist (`[server] cors_origins`, `APP__SERVER__CORS_ORIGINS`).
    pub cors_origins: Vec<String>,
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
    let workspace_routes = Router::new()
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
        .route("/proposals/{id}/unaccept", post(proposals::unaccept))
        .route("/proposals/{id}/unreject", post(proposals::unreject))
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
        .route("/apps", get(apps::list).post(apps::create))
        .route("/apps/{app}", get(apps::get))
        .route(
            "/apps/{app}/versions",
            get(apps::list_versions).post(apps::create_version),
        )
        .route("/apps/{app}/versions/{version}", get(apps::get_version))
        .route(
            "/apps/{app}/versions/{version}/publish",
            post(apps::publish),
        )
        .route(
            "/apps/{app}/versions/{version}/discard",
            post(apps::discard),
        )
        .route("/apps/{app}/rollback", post(apps::rollback))
        .route("/apps/{app}/chat", post(chat::handlers::stream_chat))
        .route(
            "/apps/{app}/conversations",
            get(chat::handlers::list_conversations),
        )
        .route(
            "/apps/{app}/conversations/{conversation_id}/messages",
            get(chat::handlers::get_messages),
        )
        .route(
            "/apps/{app}/chat/actions/{action_id}/undo",
            post(chat::handlers::undo_action),
        )
        .route("/queries", get(queries::list).post(queries::create))
        .route("/queries/{id}", get(queries::get))
        .route("/queries/{id}/run", post(query_runner::run))
        .route("/schema", get(schema::get_catalog))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            workspace_db::scope,
        ));

    let cors = cors_layer(&state.cors_origins);

    Router::new()
        .route("/health", get(health))
        .route("/openapi.json", get(openapi))
        .route(
            "/app-definition.schema.json",
            get(app_definition::schema_json),
        )
        .route("/me", get(me::get))
        .route(
            "/workspaces",
            get(me::list_workspaces).post(me::create_workspace),
        )
        .nest("/w/{workspace_id}", workspace_routes)
        .layer(cors)
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}

/// The CORS allowlist (plan B9): an allowed origin gets `Access-Control-
/// Allow-Origin` echoed back and the preflight gets `Access-Control-Max-
/// Age`; an unlisted origin gets neither. No credentials or cookies --
/// auth is a bearer token, carried in `Authorization`, which (with
/// `Content-Type`) is the only header this allows.
fn cors_layer(origins: &[String]) -> tower_http::cors::CorsLayer {
    use axum::http::Method;
    use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
    use tower_http::cors::{AllowOrigin, CorsLayer};

    let origins: Vec<axum::http::HeaderValue> = origins
        .iter()
        .filter_map(|o| axum::http::HeaderValue::from_str(o).ok())
        .collect();

    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ])
        .allow_headers([AUTHORIZATION, CONTENT_TYPE])
        .max_age(std::time::Duration::from_hours(24))
}

#[derive(Serialize, ToSchema)]
pub struct Health {
    pub status: &'static str,
}

/// Liveness, plus a control-plane round trip when one is configured.
#[utoipa::path(
    get,
    path = "/health",
    responses((status = 200, body = Health), (status = 500, body = error::ErrorBody)),
    tag = "health"
)]
pub async fn health(State(state): State<AppState>) -> AppResult<Json<Health>> {
    if let Some(control) = &state.control {
        sqlx::query("SELECT 1").execute(control).await?;
    }
    Ok(Json(Health { status: "ok" }))
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Lula.dog API",
        description = "Sales opportunity tracker and app platform"
    ),
    paths(
        health,
        me::get,
        me::list_workspaces,
        me::create_workspace,
        app_definition::schema_json,
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
        proposals::unaccept,
        proposals::unreject,
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
        apps::list,
        apps::create,
        apps::get,
        apps::list_versions,
        apps::create_version,
        apps::get_version,
        apps::publish,
        apps::discard,
        apps::rollback,
        chat::handlers::stream_chat,
        chat::handlers::list_conversations,
        chat::handlers::get_messages,
        chat::handlers::undo_action,
        queries::list,
        queries::create,
        queries::get,
        query_runner::run,
        schema::get_catalog,
    )
)]
pub struct ApiDoc;

/// The OpenAPI document for this API.
async fn openapi() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}
