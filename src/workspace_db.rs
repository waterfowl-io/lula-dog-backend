//! Per-workspace SQLite: the registry of pools keyed by workspace id, and
//! the `/w/{workspace_id}/...` middleware + extractor that resolve a
//! request into one.
//!
//! Membership is normally resolved from the control plane (Postgres); if no
//! control-plane URL is configured (`Config::control_database_url`), every
//! workspace id is treated as accessible with the `owner` role -- a
//! deliberate, documented bypass (see `docs/architecture.md`) so the data
//! plane and its tests work without standing up Postgres, the same way
//! `auth.mode = "dev"` bypasses JWT verification.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{FromRequestParts, Path, Request};
use axum::http::Method;
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::Response;
use serde::Deserialize;
use sqlx::SqlitePool;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::AppState;
use crate::auth::AuthUser;
use crate::control::workspaces::{self, Role};
use crate::db;
use crate::error::{AppError, AppResult};

#[derive(Clone)]
pub struct WorkspaceRegistry {
    data_dir: PathBuf,
    pools: Arc<Mutex<HashMap<Uuid, SqlitePool>>>,
}

impl WorkspaceRegistry {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            pools: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn path_for(&self, id: Uuid) -> PathBuf {
        self.data_dir.join("workspaces").join(format!("{id}.db"))
    }

    fn url_for(&self, id: Uuid) -> String {
        format!("sqlite://{}", self.path_for(id).display())
    }

    /// The on-disk file path for `id`'s SQLite database, for the DuckDB
    /// query runner (which attaches it directly, bypassing sqlx). Only
    /// meaningful after `get_or_create` has run for `id` at least once.
    pub fn sqlite_path(&self, id: Uuid) -> PathBuf {
        let url = self.url_for(id);
        PathBuf::from(url.strip_prefix("sqlite://").unwrap_or(&url))
    }

    /// The pool for `id`, creating and migrating its SQLite file on first
    /// use. Cached for the process lifetime; no eviction yet, which is fine
    /// at the scale of a handful of workspaces per process (see
    /// docs/architecture.md for the tradeoff).
    pub async fn get_or_create(&self, id: Uuid) -> anyhow::Result<SqlitePool> {
        let mut pools = self.pools.lock().await;
        if let Some(pool) = pools.get(&id) {
            return Ok(pool.clone());
        }
        let url = self.url_for(id);
        if let Some(path) = url.strip_prefix("sqlite://")
            && let Some(parent) = std::path::Path::new(path).parent()
        {
            tokio::fs::create_dir_all(parent).await?;
        }
        let pool = db::connect(&url).await?;
        pools.insert(id, pool.clone());
        Ok(pool)
    }
}

/// Attached to request extensions by `scope` once membership (or the
/// no-control-plane bypass) has been resolved.
#[derive(Clone)]
pub struct WorkspaceContext {
    pub id: Uuid,
    pub role: Role,
    pub pool: SqlitePool,
}

impl FromRequestParts<AppState> for WorkspaceContext {
    type Rejection = AppError;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> impl std::future::Future<Output = Result<Self, AppError>> {
        std::future::ready(parts.extensions.get::<Self>().cloned().ok_or_else(|| {
            AppError::Internal("workspace middleware did not run for this route".to_owned())
        }))
    }
}

/// Just the `workspace_id` segment. A named-field struct, not a bare
/// `Path<Uuid>`, because routes under `/w/{workspace_id}` capture further
/// segments too (e.g. `/w/{workspace_id}/opportunities/{id}`); `Path`
/// matches a struct's fields by name and ignores the rest.
#[derive(Deserialize)]
pub struct WorkspaceIdParam {
    workspace_id: Uuid,
}

/// `axum::middleware::from_fn_with_state` layer for every `/w/{workspace_id}`
/// route: resolves membership, 403s a non-member, a viewer attempting any
/// write, or a member attempting a DELETE, and opens the workspace's SQLite
/// pool.
pub async fn scope(
    auth_user: AuthUser,
    Path(WorkspaceIdParam { workspace_id }): Path<WorkspaceIdParam>,
    axum::extract::State(state): axum::extract::State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let role = resolve_role(&state, workspace_id, auth_user.id).await?;
    if *request.method() == Method::DELETE {
        // Nolan's matrix (2026-10-25): `admin`+ deletes ANY workspace-level
        // resource, not just invites/members -- see `Role`'s doc comment.
        role.require(Role::Admin)?;
    } else if !matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    ) {
        role.require(Role::Member)?;
    }
    let pool = state
        .workspaces
        .get_or_create(workspace_id)
        .await
        .map_err(|e| AppError::Internal(format!("opening workspace database: {e}")))?;
    request.extensions_mut().insert(WorkspaceContext {
        id: workspace_id,
        role,
        pool,
    });
    Ok(next.run(request).await)
}

/// The caller's role in a workspace, or the no-control-plane `owner`
/// bypass -- shared with `crate::members`, which enforces its own
/// admin-or-above gate on top of this membership check for
/// `/workspaces/{id}/invites|members|transfer-ownership`.
pub(crate) async fn resolve_role(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
) -> AppResult<Role> {
    match &state.control {
        Some(control) => workspaces::role_for(control, workspace_id, user_id)
            .await?
            .ok_or_else(|| {
                AppError::Forbidden(format!("not a member of workspace {workspace_id}"))
            }),
        None => Ok(Role::Owner),
    }
}
