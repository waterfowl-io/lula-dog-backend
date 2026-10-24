//! `/w/{workspace_id}/apps`: app definitions as data. See
//! `crate::app_definition` for the shape/validation of a definition and
//! `crate::control::app_definitions` for storage.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::AppState;
use crate::app_definition::{self, AppDefinitionDoc};
use crate::control::ControlPool;
use crate::control::app_definitions::{self, AppDefinition, AppDefinitionVersion};
use crate::error::{AppError, AppResult};
use crate::extract::ApiJson;
use crate::workspace_db::WorkspaceContext;

fn control(state: &AppState) -> AppResult<&ControlPool> {
    state
        .control
        .as_ref()
        .ok_or_else(|| AppError::Internal("CONTROL_DATABASE_URL is not configured".to_owned()))
}

/// Parse and semantically validate a definition payload, returning it ready
/// to store.
async fn parse_and_validate(
    control: &ControlPool,
    workspace_id: Uuid,
    definition: Value,
) -> AppResult<Value> {
    let doc: AppDefinitionDoc = serde_json::from_value(definition.clone())
        .map_err(|e| AppError::BadRequest(format!("invalid app definition: {e}")))?;
    app_definition::validate(control, workspace_id, &doc).await?;
    Ok(definition)
}

/// List the workspace's app definitions.
#[utoipa::path(
    get,
    path = "/w/{workspace_id}/apps",
    params(("workspace_id" = Uuid, Path)),
    responses((status = 200, body = [AppDefinition])),
    tag = "apps"
)]
pub async fn list(
    State(state): State<AppState>,
    ws: WorkspaceContext,
) -> AppResult<Json<Vec<AppDefinition>>> {
    Ok(Json(app_definitions::list(control(&state)?, ws.id).await?))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewApp {
    /// A stable slug, unique within the workspace.
    pub key: String,
    pub name: String,
}

/// Create an app definition (with no versions yet).
#[utoipa::path(
    post,
    path = "/w/{workspace_id}/apps",
    params(("workspace_id" = Uuid, Path)),
    request_body = NewApp,
    responses(
        (status = 201, body = AppDefinition),
        (status = 400, body = crate::error::ErrorBody),
        (status = 409, body = crate::error::ErrorBody),
    ),
    tag = "apps"
)]
pub async fn create(
    State(state): State<AppState>,
    ws: WorkspaceContext,
    ApiJson(new): ApiJson<NewApp>,
) -> AppResult<(StatusCode, Json<AppDefinition>)> {
    let key = new.key.trim();
    let name = new.name.trim();
    if key.is_empty() || name.is_empty() {
        return Err(AppError::BadRequest(
            "key and name cannot be empty".to_owned(),
        ));
    }
    let created = app_definitions::create(control(&state)?, ws.id, key, name).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AppDetail {
    pub app: AppDefinition,
    pub version: AppDefinitionVersion,
}

#[derive(Deserialize)]
pub struct AppIdParam {
    /// Either the app definition's `id` (UUID) or its workspace-unique
    /// `key`; see `app_definitions::resolve_id`.
    app: String,
}

/// The published version, or the latest if nothing is published yet.
#[utoipa::path(
    get,
    path = "/w/{workspace_id}/apps/{app}",
    params(
        ("workspace_id" = Uuid, Path),
        ("app" = String, Path, description = "an app definition id (UUID) or its workspace-unique key"),
    ),
    responses(
        (status = 200, body = AppDetail),
        (status = 404, body = crate::error::ErrorBody),
    ),
    tag = "apps"
)]
pub async fn get(
    State(state): State<AppState>,
    ws: WorkspaceContext,
    Path(AppIdParam { app }): Path<AppIdParam>,
) -> AppResult<Json<AppDetail>> {
    let control = control(&state)?;
    let app = app_definitions::resolve_id(control, ws.id, &app).await?;
    let (app, version) = app_definitions::fetch_published_or_latest(control, ws.id, app).await?;
    Ok(Json(AppDetail { app, version }))
}

/// All versions of an app definition, oldest first.
#[utoipa::path(
    get,
    path = "/w/{workspace_id}/apps/{app}/versions",
    params(
        ("workspace_id" = Uuid, Path),
        ("app" = String, Path, description = "an app definition id (UUID) or its workspace-unique key"),
    ),
    responses((status = 200, body = [AppDefinitionVersion])),
    tag = "apps"
)]
pub async fn list_versions(
    State(state): State<AppState>,
    ws: WorkspaceContext,
    Path(AppIdParam { app }): Path<AppIdParam>,
) -> AppResult<Json<Vec<AppDefinitionVersion>>> {
    let control = control(&state)?;
    let app = app_definitions::resolve_id(control, ws.id, &app).await?;
    // 404 if the app itself doesn't exist in this workspace.
    app_definitions::fetch(control, ws.id, app).await?;
    Ok(Json(app_definitions::list_versions(control, app).await?))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewVersion {
    #[schema(value_type = Object)]
    pub definition: Value,
}

/// Create a new (immutable) version. Validated before it's stored:
/// well-formed per the schema, unique ids, every saved-query/action
/// reference resolves.
#[utoipa::path(
    post,
    path = "/w/{workspace_id}/apps/{app}/versions",
    params(
        ("workspace_id" = Uuid, Path),
        ("app" = String, Path, description = "an app definition id (UUID) or its workspace-unique key"),
    ),
    request_body = NewVersion,
    responses(
        (status = 201, body = AppDefinitionVersion),
        (status = 400, body = crate::error::ErrorBody),
        (status = 404, body = crate::error::ErrorBody),
    ),
    tag = "apps"
)]
pub async fn create_version(
    State(state): State<AppState>,
    ws: WorkspaceContext,
    Path(AppIdParam { app }): Path<AppIdParam>,
    ApiJson(new): ApiJson<NewVersion>,
) -> AppResult<(StatusCode, Json<AppDefinitionVersion>)> {
    let control = control(&state)?;
    let app = app_definitions::resolve_id(control, ws.id, &app).await?;
    app_definitions::fetch(control, ws.id, app).await?;
    let definition = parse_and_validate(control, ws.id, new.definition).await?;
    let version = app_definitions::create_version(control, app, definition, None).await?;
    Ok((StatusCode::CREATED, Json(version)))
}

#[derive(Deserialize)]
pub struct PublishParam {
    /// Either the app definition's `id` (UUID) or its workspace-unique
    /// `key`; see `app_definitions::resolve_id`.
    app: String,
    version: Uuid,
}

/// Move the published pointer to `version` (must belong to this app).
#[utoipa::path(
    post,
    path = "/w/{workspace_id}/apps/{app}/versions/{version}/publish",
    params(
        ("workspace_id" = Uuid, Path),
        ("app" = String, Path, description = "an app definition id (UUID) or its workspace-unique key"),
        ("version" = Uuid, Path, description = "an app_definition_versions id"),
    ),
    responses(
        (status = 200, body = AppDefinition),
        (status = 400, body = crate::error::ErrorBody),
        (status = 404, body = crate::error::ErrorBody),
    ),
    tag = "apps"
)]
pub async fn publish(
    State(state): State<AppState>,
    ws: WorkspaceContext,
    Path(PublishParam { app, version }): Path<PublishParam>,
) -> AppResult<Json<AppDefinition>> {
    let control = control(&state)?;
    let app = app_definitions::resolve_id(control, ws.id, &app).await?;
    Ok(Json(
        app_definitions::publish(control, ws.id, app, version).await?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RollbackRequest {
    /// The version NUMBER (`app_definition_versions.version`) to roll back
    /// to, not its id.
    pub version: i32,
}

/// Roll back to an earlier version: copies its `definition` into a brand
/// new version (history stays an immutable, append-only log) and publishes
/// that new version.
#[utoipa::path(
    post,
    path = "/w/{workspace_id}/apps/{app}/rollback",
    params(
        ("workspace_id" = Uuid, Path),
        ("app" = String, Path, description = "an app definition id (UUID) or its workspace-unique key"),
    ),
    request_body = RollbackRequest,
    responses(
        (status = 200, body = AppDefinitionVersion),
        (status = 404, body = crate::error::ErrorBody),
    ),
    tag = "apps"
)]
pub async fn rollback(
    State(state): State<AppState>,
    ws: WorkspaceContext,
    Path(AppIdParam { app }): Path<AppIdParam>,
    ApiJson(req): ApiJson<RollbackRequest>,
) -> AppResult<Json<AppDefinitionVersion>> {
    let control = control(&state)?;
    let app = app_definitions::resolve_id(control, ws.id, &app).await?;
    app_definitions::fetch(control, ws.id, app).await?;
    let old = app_definitions::fetch_version(control, app, req.version).await?;
    let new_version = app_definitions::create_version(control, app, old.definition, None).await?;
    app_definitions::publish(control, ws.id, app, new_version.id).await?;
    Ok(Json(new_version))
}
