//! `/w/{workspace_id}/queries`: saved queries. Execution is
//! `crate::query_runner`.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use uuid::Uuid;

use crate::AppState;
use crate::control::ControlPool;
use crate::control::saved_queries::{self, NewSavedQuery, SavedQuery};
use crate::error::{AppError, AppResult};
use crate::extract::ApiJson;
use crate::workspace_db::WorkspaceContext;

pub(crate) fn control(state: &AppState) -> AppResult<&ControlPool> {
    state
        .control
        .as_ref()
        .ok_or_else(|| AppError::Internal("CONTROL_DATABASE_URL is not configured".to_owned()))
}

#[derive(Deserialize)]
pub struct QueryIdParam {
    pub id: Uuid,
}

/// List the workspace's saved queries.
#[utoipa::path(
    get,
    path = "/w/{workspace_id}/queries",
    params(("workspace_id" = Uuid, Path)),
    responses((status = 200, body = [SavedQuery])),
    tag = "queries"
)]
pub async fn list(
    State(state): State<AppState>,
    ws: WorkspaceContext,
) -> AppResult<Json<Vec<SavedQuery>>> {
    Ok(Json(saved_queries::list(control(&state)?, ws.id).await?))
}

/// Create a saved query.
#[utoipa::path(
    post,
    path = "/w/{workspace_id}/queries",
    params(("workspace_id" = Uuid, Path)),
    request_body = NewSavedQuery,
    responses(
        (status = 201, body = SavedQuery),
        (status = 400, body = crate::error::ErrorBody),
        (status = 409, body = crate::error::ErrorBody),
    ),
    tag = "queries"
)]
pub async fn create(
    State(state): State<AppState>,
    ws: WorkspaceContext,
    ApiJson(new): ApiJson<NewSavedQuery>,
) -> AppResult<(StatusCode, Json<SavedQuery>)> {
    let created = saved_queries::create(control(&state)?, ws.id, new).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// Fetch one saved query.
#[utoipa::path(
    get,
    path = "/w/{workspace_id}/queries/{id}",
    params(("workspace_id" = Uuid, Path), ("id" = Uuid, Path)),
    responses(
        (status = 200, body = SavedQuery),
        (status = 404, body = crate::error::ErrorBody),
    ),
    tag = "queries"
)]
pub async fn get(
    State(state): State<AppState>,
    ws: WorkspaceContext,
    Path(QueryIdParam { id }): Path<QueryIdParam>,
) -> AppResult<Json<SavedQuery>> {
    Ok(Json(
        saved_queries::fetch(control(&state)?, ws.id, id).await?,
    ))
}
