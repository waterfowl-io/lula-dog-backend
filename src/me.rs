//! `/me` and `/workspaces`: the authenticated user's own view of the
//! control plane. Not workspace-scoped: no `/w/{id}` prefix, no membership
//! check (there's no workspace yet to be a member of).

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::AppState;
use crate::auth::AuthUser;
use crate::control::ControlPool;
use crate::control::profiles::{self, Profile};
use crate::control::workspaces::{self, Workspace, WorkspaceMembership};
use crate::error::{AppError, AppResult};
use crate::extract::ApiJson;

fn control(state: &AppState) -> AppResult<&ControlPool> {
    state
        .control
        .as_ref()
        .ok_or_else(|| AppError::Internal("CONTROL_DATABASE_URL is not configured".to_owned()))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Me {
    pub profile: Profile,
    pub workspaces: Vec<WorkspaceMembership>,
}

/// The authenticated user and their workspace memberships. Also the point
/// where a profile row is first created for a newly-seen Supabase user.
#[utoipa::path(
    get,
    path = "/me",
    responses(
        (status = 200, body = Me),
        (status = 401, body = crate::error::ErrorBody),
    ),
    tag = "me"
)]
pub async fn get(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Me>> {
    let control = control(&state)?;
    let profile = profiles::upsert_from_jwt(control, user.id, &user.email).await?;
    let workspaces = workspaces::list_for_user(control, user.id).await?;
    Ok(Json(Me {
        profile,
        workspaces,
    }))
}

/// The caller's workspaces.
#[utoipa::path(
    get,
    path = "/workspaces",
    responses((status = 200, body = [WorkspaceMembership])),
    tag = "me"
)]
pub async fn list_workspaces(
    State(state): State<AppState>,
    user: AuthUser,
) -> AppResult<Json<Vec<WorkspaceMembership>>> {
    let control = control(&state)?;
    profiles::upsert_from_jwt(control, user.id, &user.email).await?;
    Ok(Json(workspaces::list_for_user(control, user.id).await?))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewWorkspace {
    pub name: String,
}

/// Create a workspace; the caller becomes its `owner`. Also creates the
/// workspace's (empty, migrated) SQLite file.
#[utoipa::path(
    post,
    path = "/workspaces",
    request_body = NewWorkspace,
    responses(
        (status = 201, body = Workspace),
        (status = 400, body = crate::error::ErrorBody),
    ),
    tag = "me"
)]
pub async fn create_workspace(
    State(state): State<AppState>,
    user: AuthUser,
    ApiJson(new): ApiJson<NewWorkspace>,
) -> AppResult<(StatusCode, Json<Workspace>)> {
    let name = new.name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("name cannot be empty".to_owned()));
    }
    let control = control(&state)?;
    profiles::upsert_from_jwt(control, user.id, &user.email).await?;
    let workspace = workspaces::create(control, name, user.id).await?;
    state
        .workspaces
        .get_or_create(workspace.id)
        .await
        .map_err(|e| AppError::Internal(format!("creating workspace database: {e}")))?;
    Ok((StatusCode::CREATED, Json(workspace)))
}
