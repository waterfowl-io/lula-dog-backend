//! Saved queries: named SQL a chart/table component binds to by key. Run
//! through DuckDB against the workspace SQLite file; see
//! `crate::query_runner`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use super::ControlPool;
use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Serialize, FromRow, ToSchema)]
pub struct SavedQuery {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub key: String,
    pub name: String,
    pub description: Option<String>,
    pub sql_text: String,
    /// `[{"name": "...", "type": "string|number|boolean"}, ...]`.
    #[schema(value_type = Object)]
    pub params: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewSavedQuery {
    pub key: String,
    pub name: String,
    pub description: Option<String>,
    pub sql_text: String,
    #[serde(default = "default_params")]
    #[schema(value_type = Object)]
    pub params: Value,
}

fn default_params() -> Value {
    Value::Array(Vec::new())
}

pub async fn list(pool: &ControlPool, workspace_id: Uuid) -> AppResult<Vec<SavedQuery>> {
    Ok(sqlx::query_as::<_, SavedQuery>(
        "SELECT * FROM saved_queries WHERE workspace_id = $1 ORDER BY key",
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await?)
}

pub async fn fetch(pool: &ControlPool, workspace_id: Uuid, id: Uuid) -> AppResult<SavedQuery> {
    sqlx::query_as::<_, SavedQuery>(
        "SELECT * FROM saved_queries WHERE workspace_id = $1 AND id = $2",
    )
    .bind(workspace_id)
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound("saved query"))
}

pub async fn find_by_key(
    pool: &ControlPool,
    workspace_id: Uuid,
    key: &str,
) -> AppResult<Option<SavedQuery>> {
    Ok(sqlx::query_as::<_, SavedQuery>(
        "SELECT * FROM saved_queries WHERE workspace_id = $1 AND key = $2",
    )
    .bind(workspace_id)
    .bind(key)
    .fetch_optional(pool)
    .await?)
}

/// Resolve a path segment that may be either a saved query's `id` (UUID) or
/// its workspace-unique `key` into an id. Mirrors
/// `crate::control::app_definitions::resolve_id`: app definitions reference
/// saved queries by key (`QueryBinding::query`), but the query runner
/// endpoint takes an id, so the frontend runtime hits this on every query
/// a component binds to.
pub async fn resolve_id(pool: &ControlPool, workspace_id: Uuid, raw: &str) -> AppResult<Uuid> {
    if let Ok(id) = raw.parse::<Uuid>() {
        return Ok(id);
    }
    find_by_key(pool, workspace_id, raw)
        .await?
        .map(|query| query.id)
        .ok_or(AppError::NotFound("saved query"))
}

pub async fn create(
    pool: &ControlPool,
    workspace_id: Uuid,
    new: NewSavedQuery,
) -> AppResult<SavedQuery> {
    if new.sql_text.trim().is_empty() {
        return Err(AppError::BadRequest("sql_text cannot be empty".to_owned()));
    }
    if find_by_key(pool, workspace_id, &new.key).await?.is_some() {
        return Err(AppError::Conflict(format!(
            "a saved query with key `{}` already exists in this workspace",
            new.key
        )));
    }
    Ok(sqlx::query_as::<_, SavedQuery>(
        "INSERT INTO saved_queries (workspace_id, key, name, description, sql_text, params)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING *",
    )
    .bind(workspace_id)
    .bind(new.key)
    .bind(new.name)
    .bind(new.description)
    .bind(new.sql_text)
    .bind(new.params)
    .fetch_one(pool)
    .await?)
}

/// Insert or update by `(workspace_id, key)`. Used by `cargo run -- seed`
/// so re-running it against a workspace that already has these queries
/// converges them to the current definitions instead of only creating
/// whatever's missing.
pub async fn upsert(
    pool: &ControlPool,
    workspace_id: Uuid,
    new: NewSavedQuery,
) -> AppResult<SavedQuery> {
    if new.sql_text.trim().is_empty() {
        return Err(AppError::BadRequest("sql_text cannot be empty".to_owned()));
    }
    Ok(sqlx::query_as::<_, SavedQuery>(
        "INSERT INTO saved_queries (workspace_id, key, name, description, sql_text, params)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (workspace_id, key) DO UPDATE
         SET name = EXCLUDED.name,
             description = EXCLUDED.description,
             sql_text = EXCLUDED.sql_text,
             params = EXCLUDED.params,
             updated_at = now()
         RETURNING *",
    )
    .bind(workspace_id)
    .bind(new.key)
    .bind(new.name)
    .bind(new.description)
    .bind(new.sql_text)
    .bind(new.params)
    .fetch_one(pool)
    .await?)
}
