//! Tasks on an opportunity: added by hand or suggested by the extractor from
//! a pasted thread (then `source_entry_id` points at that entry).

use axum::Json;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqliteConnection, SqlitePool};
use utoipa::ToSchema;

use crate::error::{AppError, AppResult};
use crate::extract::{ApiJson, ApiPath, IdPath};
use crate::opportunities::{self, not_null, nullable};
use crate::workspace_db::WorkspaceContext;

#[derive(Debug, Clone, Serialize, FromRow, ToSchema)]
pub struct Task {
    pub id: i64,
    pub opportunity_id: i64,
    pub title: String,
    /// `YYYY-MM-DD HH:MM:SS` UTC. Overdue once its calendar day has passed.
    pub due_at: Option<String>,
    pub done: bool,
    /// The timeline entry this task was suggested from, if any.
    pub source_entry_id: Option<i64>,
}

fn title(raw: &str) -> AppResult<String> {
    let t = raw.trim();
    if t.is_empty() {
        return Err(AppError::BadRequest("title cannot be empty".to_owned()));
    }
    Ok(t.to_owned())
}

fn due_at(raw: Option<&str>) -> AppResult<Option<String>> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            crate::time::normalize(s).ok_or_else(|| {
                AppError::BadRequest(format!("due_at is not a recognized timestamp: {s:?}"))
            })
        })
        .transpose()
}

/// Validate and insert an open task. The one writer of task rows; the paste
/// flow and the seed command use it too.
pub async fn insert(
    conn: &mut SqliteConnection,
    opportunity_id: i64,
    raw_title: &str,
    raw_due_at: Option<&str>,
    source_entry_id: Option<i64>,
) -> AppResult<Task> {
    Ok(sqlx::query_as::<_, Task>(
        "INSERT INTO tasks (opportunity_id, title, due_at, done, source_entry_id)
         VALUES (?1, ?2, ?3, 0, ?4)
         RETURNING *",
    )
    .bind(opportunity_id)
    .bind(title(raw_title)?)
    .bind(due_at(raw_due_at)?)
    .bind(source_entry_id)
    .fetch_one(conn)
    .await?)
}

pub async fn list_for(conn: &mut SqliteConnection, opportunity_id: i64) -> AppResult<Vec<Task>> {
    Ok(sqlx::query_as::<_, Task>(
        "SELECT * FROM tasks WHERE opportunity_id = ?1
         ORDER BY done, due_at IS NULL, due_at, id",
    )
    .bind(opportunity_id)
    .fetch_all(conn)
    .await?)
}

pub(crate) async fn fetch(conn: &mut SqliteConnection, id: i64) -> AppResult<Task> {
    sqlx::query_as::<_, Task>("SELECT * FROM tasks WHERE id = ?1")
        .bind(id)
        .fetch_optional(conn)
        .await?
        .ok_or(AppError::NotFound("task"))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewTask {
    pub title: String,
    pub due_at: Option<String>,
}

/// Add a task to an opportunity.
#[utoipa::path(
    post,
    path = "/w/{workspace_id}/opportunities/{id}/tasks",
    params(("workspace_id" = Uuid, Path), ("id" = i64, Path)),
    request_body = NewTask,
    responses(
        (status = 201, body = Task),
        (status = 400, body = crate::error::ErrorBody),
        (status = 404, body = crate::error::ErrorBody),
    ),
    tag = "tasks"
)]
pub async fn create(
    ws: WorkspaceContext,
    ApiPath(IdPath { id: opportunity_id }): ApiPath<IdPath>,
    ApiJson(new): ApiJson<NewTask>,
) -> AppResult<(StatusCode, Json<Task>)> {
    let task = create_via_pool(&ws.pool, opportunity_id, &new.title, new.due_at.as_deref()).await?;
    Ok((StatusCode::CREATED, Json(task)))
}

/// `insert` plus the opportunity-exists check, on a borrowed connection --
/// the one entry point both the HTTP handler (via `create_via_pool`) and
/// the workspace actor's `crate::actions::dispatch_in` (chat's
/// `task.create` tool) call.
pub async fn create_in(
    conn: &mut SqliteConnection,
    opportunity_id: i64,
    raw_title: &str,
    raw_due_at: Option<&str>,
) -> AppResult<Task> {
    opportunities::fetch(conn, opportunity_id).await?;
    insert(conn, opportunity_id, raw_title, raw_due_at, None).await
}

/// `create_in`, from a pool rather than a borrowed connection, in its own
/// transaction.
pub async fn create_via_pool(
    pool: &SqlitePool,
    opportunity_id: i64,
    raw_title: &str,
    raw_due_at: Option<&str>,
) -> AppResult<Task> {
    let mut tx = pool.begin().await?;
    let task = create_in(&mut tx, opportunity_id, raw_title, raw_due_at).await?;
    tx.commit().await?;
    Ok(task)
}

/// A partial update. `"due_at": null` clears the due date.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskPatch {
    #[serde(default, deserialize_with = "not_null")]
    pub title: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub due_at: Option<Option<String>>,
    #[serde(default, deserialize_with = "not_null")]
    pub done: Option<bool>,
}

/// Edit a task or mark it done.
#[utoipa::path(
    patch,
    path = "/w/{workspace_id}/tasks/{id}",
    params(("workspace_id" = Uuid, Path), ("id" = i64, Path)),
    request_body = TaskPatch,
    responses(
        (status = 200, body = Task),
        (status = 400, body = crate::error::ErrorBody),
        (status = 404, body = crate::error::ErrorBody),
    ),
    tag = "tasks"
)]
pub async fn patch(
    ws: WorkspaceContext,
    ApiPath(IdPath { id }): ApiPath<IdPath>,
    ApiJson(patch): ApiJson<TaskPatch>,
) -> AppResult<Json<Task>> {
    let (_old, new) = apply_patch_via_pool(&ws.pool, id, patch).await?;
    Ok(Json(new))
}

/// `patch`'s logic, on a borrowed connection, also returning the task as it
/// stood before the patch -- what an undo (chat's `task.complete` tool)
/// needs to revert `done`. The one entry point both the HTTP handler (via
/// `apply_patch_via_pool`) and the workspace actor's
/// `crate::actions::dispatch_in`/`undo_in` call.
pub async fn apply_patch_in(
    conn: &mut SqliteConnection,
    id: i64,
    patch: TaskPatch,
) -> AppResult<(Task, Task)> {
    let TaskPatch {
        title: patch_title,
        due_at: patch_due_at,
        done: patch_done,
    } = patch;
    let current = fetch(conn, id).await?;
    let new_title = match patch_title {
        Some(t) => title(&t)?,
        None => current.title.clone(),
    };
    let new_due_at = match patch_due_at {
        Some(d) => due_at(d.as_deref())?,
        None => current.due_at.clone(),
    };
    let new_done = patch_done.unwrap_or(current.done);
    let new = sqlx::query_as::<_, Task>(
        "UPDATE tasks SET title = ?1, due_at = ?2, done = ?3 WHERE id = ?4 RETURNING *",
    )
    .bind(new_title)
    .bind(new_due_at)
    .bind(new_done)
    .bind(id)
    .fetch_one(conn)
    .await?;
    Ok((current, new))
}

/// `apply_patch_in`, from a pool rather than a borrowed connection, in its
/// own transaction.
pub async fn apply_patch_via_pool(
    pool: &SqlitePool,
    id: i64,
    patch: TaskPatch,
) -> AppResult<(Task, Task)> {
    let mut tx = pool.begin().await?;
    let result = apply_patch_in(&mut tx, id, patch).await?;
    tx.commit().await?;
    Ok(result)
}

/// Delete a task.
#[utoipa::path(
    delete,
    path = "/w/{workspace_id}/tasks/{id}",
    params(("workspace_id" = Uuid, Path), ("id" = i64, Path)),
    responses(
        (status = 204),
        (status = 404, body = crate::error::ErrorBody),
    ),
    tag = "tasks"
)]
pub async fn delete(
    ws: WorkspaceContext,
    ApiPath(IdPath { id }): ApiPath<IdPath>,
) -> AppResult<StatusCode> {
    delete_via_pool(&ws.pool, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `delete`'s logic, on a borrowed connection -- also used to undo chat's
/// `task.create` tool (undoing a task creation deletes it outright; there
/// is no soft-delete state to go back to).
pub async fn delete_in(conn: &mut SqliteConnection, id: i64) -> AppResult<()> {
    let deleted = sqlx::query("DELETE FROM tasks WHERE id = ?1")
        .bind(id)
        .execute(conn)
        .await?
        .rows_affected();
    if deleted == 0 {
        return Err(AppError::NotFound("task"));
    }
    Ok(())
}

/// `delete_in`, from a pool rather than a borrowed connection, in its own
/// transaction.
pub async fn delete_via_pool(pool: &SqlitePool, id: i64) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    delete_in(&mut tx, id).await?;
    tx.commit().await?;
    Ok(())
}
