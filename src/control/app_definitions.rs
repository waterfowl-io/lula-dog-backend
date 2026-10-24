//! App definitions: an AI-generated app (pages/layout/components) as data.
//! Versions are immutable; `published_version_id` is the pointer the
//! frontend reads by default. See `crate::app_definition` for the shape of
//! `definition` and its validation, and docs/architecture.md for the
//! rollback design (a new version copied from an old one, then published --
//! the published pointer only ever moves forward through real history).

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use super::ControlPool;
use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Serialize, FromRow, ToSchema)]
pub struct AppDefinition {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub key: String,
    pub name: String,
    pub published_version_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, FromRow, ToSchema)]
pub struct AppDefinitionVersion {
    pub id: Uuid,
    pub app_definition_id: Uuid,
    pub version: i32,
    #[schema(value_type = Object)]
    pub definition: Value,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

pub async fn list(pool: &ControlPool, workspace_id: Uuid) -> AppResult<Vec<AppDefinition>> {
    Ok(sqlx::query_as::<_, AppDefinition>(
        "SELECT * FROM app_definitions WHERE workspace_id = $1 ORDER BY created_at",
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await?)
}

pub async fn fetch(pool: &ControlPool, workspace_id: Uuid, id: Uuid) -> AppResult<AppDefinition> {
    sqlx::query_as::<_, AppDefinition>(
        "SELECT * FROM app_definitions WHERE workspace_id = $1 AND id = $2",
    )
    .bind(workspace_id)
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound("app definition"))
}

pub async fn find_by_key(
    pool: &ControlPool,
    workspace_id: Uuid,
    key: &str,
) -> AppResult<Option<AppDefinition>> {
    Ok(sqlx::query_as::<_, AppDefinition>(
        "SELECT * FROM app_definitions WHERE workspace_id = $1 AND key = $2",
    )
    .bind(workspace_id)
    .bind(key)
    .fetch_optional(pool)
    .await?)
}

/// Create an app definition with no versions yet.
pub async fn create(
    pool: &ControlPool,
    workspace_id: Uuid,
    key: &str,
    name: &str,
) -> AppResult<AppDefinition> {
    if sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM app_definitions WHERE workspace_id = $1 AND key = $2)",
    )
    .bind(workspace_id)
    .bind(key)
    .fetch_one(pool)
    .await?
    {
        return Err(AppError::Conflict(format!(
            "an app definition with key `{key}` already exists in this workspace"
        )));
    }
    Ok(sqlx::query_as::<_, AppDefinition>(
        "INSERT INTO app_definitions (workspace_id, key, name) VALUES ($1, $2, $3) RETURNING *",
    )
    .bind(workspace_id)
    .bind(key)
    .bind(name)
    .fetch_one(pool)
    .await?)
}

pub async fn list_versions(
    pool: &ControlPool,
    app_definition_id: Uuid,
) -> AppResult<Vec<AppDefinitionVersion>> {
    Ok(sqlx::query_as::<_, AppDefinitionVersion>(
        "SELECT * FROM app_definition_versions WHERE app_definition_id = $1 ORDER BY version",
    )
    .bind(app_definition_id)
    .fetch_all(pool)
    .await?)
}

pub async fn fetch_version(
    pool: &ControlPool,
    app_definition_id: Uuid,
    version: i32,
) -> AppResult<AppDefinitionVersion> {
    sqlx::query_as::<_, AppDefinitionVersion>(
        "SELECT * FROM app_definition_versions WHERE app_definition_id = $1 AND version = $2",
    )
    .bind(app_definition_id)
    .bind(version)
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound("app definition version"))
}

/// Create the next version (`definition` must already be validated). Locks
/// the parent `app_definitions` row for the duration of the transaction so
/// two concurrent creates can't compute the same next version number.
pub async fn create_version(
    pool: &ControlPool,
    app_definition_id: Uuid,
    definition: Value,
    created_by: Option<Uuid>,
) -> AppResult<AppDefinitionVersion> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT id FROM app_definitions WHERE id = $1 FOR UPDATE")
        .bind(app_definition_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(AppError::NotFound("app definition"))?;
    let version: AppDefinitionVersion = sqlx::query_as(
        "INSERT INTO app_definition_versions (app_definition_id, version, definition, created_by)
         SELECT $1, COALESCE(MAX(version), 0) + 1, $2, $3
         FROM app_definition_versions WHERE app_definition_id = $1
         RETURNING *",
    )
    .bind(app_definition_id)
    .bind(definition)
    .bind(created_by)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(version)
}

/// Move the published pointer to `version_id`, which must belong to `id`.
pub async fn publish(
    pool: &ControlPool,
    workspace_id: Uuid,
    id: Uuid,
    version_id: Uuid,
) -> AppResult<AppDefinition> {
    let owns = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM app_definition_versions WHERE id = $1 AND app_definition_id = $2)",
    )
    .bind(version_id)
    .bind(id)
    .fetch_one(pool)
    .await?;
    if !owns {
        return Err(AppError::BadRequest(
            "version does not belong to this app definition".to_owned(),
        ));
    }
    sqlx::query_as::<_, AppDefinition>(
        "UPDATE app_definitions SET published_version_id = $1, updated_at = now()
         WHERE workspace_id = $2 AND id = $3
         RETURNING *",
    )
    .bind(version_id)
    .bind(workspace_id)
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound("app definition"))
}

/// The published version, or the latest if nothing is published yet.
pub async fn fetch_published_or_latest(
    pool: &ControlPool,
    workspace_id: Uuid,
    id: Uuid,
) -> AppResult<(AppDefinition, AppDefinitionVersion)> {
    let app = fetch(pool, workspace_id, id).await?;
    let version = if let Some(version_id) = app.published_version_id {
        sqlx::query_as::<_, AppDefinitionVersion>(
            "SELECT * FROM app_definition_versions WHERE id = $1",
        )
        .bind(version_id)
        .fetch_optional(pool)
        .await?
    } else {
        sqlx::query_as::<_, AppDefinitionVersion>(
            "SELECT * FROM app_definition_versions WHERE app_definition_id = $1
             ORDER BY version DESC LIMIT 1",
        )
        .bind(id)
        .fetch_optional(pool)
        .await?
    };
    let version = version.ok_or(AppError::NotFound("app definition version"))?;
    Ok((app, version))
}
