//! Tasks on an opportunity: added by hand or suggested by the extractor from
//! a pasted thread (then `source_entry_id` points at that entry).

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqliteConnection};
use utoipa::ToSchema;

use crate::AppState;
use crate::error::{AppError, AppResult};
use crate::extract::{ApiJson, ApiPath};
use crate::opportunities::{self, not_null, nullable};

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

async fn fetch(conn: &mut SqliteConnection, id: i64) -> AppResult<Task> {
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
    path = "/opportunities/{id}/tasks",
    params(("id" = i64, Path)),
    request_body = NewTask,
    responses(
        (status = 201, body = Task),
        (status = 400, body = crate::error::ErrorBody),
        (status = 404, body = crate::error::ErrorBody),
    ),
    tag = "tasks"
)]
pub async fn create(
    State(state): State<AppState>,
    ApiPath(opportunity_id): ApiPath<i64>,
    ApiJson(new): ApiJson<NewTask>,
) -> AppResult<(StatusCode, Json<Task>)> {
    let mut conn = state.pool.acquire().await?;
    opportunities::fetch(&mut conn, opportunity_id).await?;
    let task = insert(
        &mut conn,
        opportunity_id,
        &new.title,
        new.due_at.as_deref(),
        None,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(task)))
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
    path = "/tasks/{id}",
    params(("id" = i64, Path)),
    request_body = TaskPatch,
    responses(
        (status = 200, body = Task),
        (status = 400, body = crate::error::ErrorBody),
        (status = 404, body = crate::error::ErrorBody),
    ),
    tag = "tasks"
)]
pub async fn patch(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<i64>,
    ApiJson(patch): ApiJson<TaskPatch>,
) -> AppResult<Json<Task>> {
    let mut conn = state.pool.acquire().await?;
    let current = fetch(&mut conn, id).await?;
    let new_title = match patch.title {
        Some(t) => title(&t)?,
        None => current.title,
    };
    let new_due_at = match patch.due_at {
        Some(d) => due_at(d.as_deref())?,
        None => current.due_at,
    };
    let new_done = patch.done.unwrap_or(current.done);
    let task = sqlx::query_as::<_, Task>(
        "UPDATE tasks SET title = ?1, due_at = ?2, done = ?3 WHERE id = ?4 RETURNING *",
    )
    .bind(new_title)
    .bind(new_due_at)
    .bind(new_done)
    .bind(id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(Json(task))
}

/// Delete a task.
#[utoipa::path(
    delete,
    path = "/tasks/{id}",
    params(("id" = i64, Path)),
    responses(
        (status = 204),
        (status = 404, body = crate::error::ErrorBody),
    ),
    tag = "tasks"
)]
pub async fn delete(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<i64>,
) -> AppResult<StatusCode> {
    let mut conn = state.pool.acquire().await?;
    let deleted = sqlx::query("DELETE FROM tasks WHERE id = ?1")
        .bind(id)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    if deleted == 0 {
        return Err(AppError::NotFound("task"));
    }
    Ok(StatusCode::NO_CONTENT)
}
