//! `POST /w/{workspace_id}/queries/{id}/run`: execute a saved query
//! read-only against the workspace's SQLite file.
//!
//! The spec target was DuckDB attaching the SQLite file read-only
//! (`ATTACH ... (TYPE SQLITE, READ_ONLY)`). In practice, `duckdb-rs`'s
//! `bundled`/`extensions-full` features compile DuckDB core plus the
//! parquet/json/core_functions extensions from source, but NOT
//! `sqlite_scanner`: DuckDB treats it as a separate ("community")
//! extension it downloads from its extension repository the first time
//! `ATTACH ... TYPE SQLITE` runs, which needs network access. That
//! conflicts with this project's no-network-in-tests/CI rule, so this
//! executes directly against the workspace's own SQLite file instead --
//! read-only at the connection level (not just by convention), with the
//! same safety checks DuckDB would have enforced: a single `SELECT`/`WITH`
//! statement, a row cap, a timeout, and named params bound (never
//! interpolated) by the driver. Revisit DuckDB once `sqlite_scanner` can be
//! statically linked, or if extension autoload over the network becomes
//! acceptable for this deployment -- see docs/architecture.md.

use std::collections::BTreeMap;
use std::path::Path as FsPath;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqliteRow};
use sqlx::{Column, Connection, Executor, Row, SqliteConnection};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::AppState;
use crate::control::saved_queries;
use crate::error::{AppError, AppResult};
use crate::extract::ApiJson;
use crate::queries::{QueryIdParam, control};
use crate::workspace_db::WorkspaceContext;

#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RunRequest {
    #[serde(default)]
    #[schema(value_type = Object)]
    pub params: BTreeMap<String, Value>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ColumnInfo {
    pub name: String,
    #[serde(rename = "type")]
    pub data_type: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RunResponse {
    pub query_id: Uuid,
    /// The query's SQL text, so the frontend can show it (the `</>` button).
    pub sql: String,
    pub columns: Vec<ColumnInfo>,
    #[schema(value_type = Vec<Vec<Object>>)]
    pub rows: Vec<Vec<Value>>,
    pub row_count: usize,
    /// `true` if there were more rows than the limit; `rows` holds only the
    /// first `row_count` of them.
    pub truncated: bool,
    pub elapsed_ms: u64,
}

/// `sql` must be exactly one `SELECT`/`WITH` statement: no trailing
/// semicolon-separated statement, nothing else.
fn ensure_single_select(sql: &str) -> AppResult<()> {
    let trimmed = sql.trim().trim_end_matches(';').trim();
    if trimmed.is_empty() {
        return Err(AppError::BadRequest("sql_text is empty".to_owned()));
    }
    if trimmed.contains(';') {
        return Err(AppError::BadRequest(
            "sql_text must be a single statement".to_owned(),
        ));
    }
    let first_word = trimmed
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if first_word != "select" && first_word != "with" {
        return Err(AppError::BadRequest(
            "sql_text must start with SELECT or WITH".to_owned(),
        ));
    }
    Ok(())
}

/// Rewrite `:name`/`$name` placeholders (SQLite's named-parameter syntax)
/// into positional `?`s, in encounter order, looking each name up in
/// `params`. This is the only parameter mechanism sqlx's runtime (non-macro)
/// query API supports, since `bind` is purely positional.
fn rewrite_named_params(
    sql: &str,
    params: &BTreeMap<String, Value>,
) -> AppResult<(String, Vec<Value>)> {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = String::with_capacity(sql.len());
    let mut bound = Vec::new();
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        let starts_name = chars
            .get(i + 1)
            .is_some_and(|next| next.is_alphabetic() || *next == '_');
        if (c == ':' || c == '$') && starts_name {
            let start = i + 1;
            let mut end = start;
            while chars
                .get(end)
                .is_some_and(|ch| ch.is_alphanumeric() || *ch == '_')
            {
                end += 1;
            }
            let name: String = chars.get(start..end).unwrap_or_default().iter().collect();
            let value = params.get(&name).ok_or_else(|| {
                AppError::BadRequest(format!("missing value for query param `{name}`"))
            })?;
            if matches!(value, Value::Array(_) | Value::Object(_)) {
                return Err(AppError::BadRequest(format!(
                    "query param `{name}` must be a string, number, boolean or null"
                )));
            }
            out.push('?');
            bound.push(value.clone());
            i = end;
        } else {
            out.push(c);
            i += 1;
        }
    }
    Ok((out, bound))
}

/// Run a saved query.
#[utoipa::path(
    post,
    path = "/w/{workspace_id}/queries/{id}/run",
    params(
        ("workspace_id" = Uuid, Path),
        ("id" = String, Path, description = "a saved query id (UUID) or its workspace-unique key"),
    ),
    request_body = RunRequest,
    responses(
        (status = 200, body = RunResponse),
        (status = 400, body = crate::error::ErrorBody),
        (status = 404, body = crate::error::ErrorBody),
    ),
    tag = "queries"
)]
pub async fn run(
    State(state): State<AppState>,
    ws: WorkspaceContext,
    Path(QueryIdParam { id }): Path<QueryIdParam>,
    ApiJson(req): ApiJson<RunRequest>,
) -> AppResult<Json<RunResponse>> {
    let control = control(&state)?;
    let id = saved_queries::resolve_id(control, ws.id, &id).await?;
    let query = saved_queries::fetch(control, ws.id, id).await?;
    ensure_single_select(&query.sql_text)?;

    let sqlite_path = state.workspaces.sqlite_path(ws.id);
    let max_rows = state.query_max_rows;
    let timeout = Duration::from_millis(state.query_timeout_ms);

    let started = Instant::now();
    let executed = tokio::time::timeout(
        timeout,
        execute(&sqlite_path, &query.sql_text, &req.params, max_rows),
    )
    .await
    .map_err(|_| AppError::BadRequest("query timed out".to_owned()))??;
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    Ok(Json(RunResponse {
        query_id: id,
        sql: query.sql_text,
        columns: executed.columns,
        row_count: executed.rows.len(),
        rows: executed.rows,
        truncated: executed.truncated,
        elapsed_ms,
    }))
}

struct Executed {
    columns: Vec<ColumnInfo>,
    rows: Vec<Vec<Value>>,
    truncated: bool,
}

async fn execute(
    sqlite_path: &FsPath,
    sql: &str,
    params: &BTreeMap<String, Value>,
    max_rows: i64,
) -> AppResult<Executed> {
    let (rewritten, bound) = rewrite_named_params(sql, params)?;
    // One extra row fetched, never returned, just to know whether to set
    // `truncated` without an additional COUNT(*) round trip.
    let limited_sql = format!(
        "SELECT * FROM ({rewritten}) AS lula_query LIMIT {}",
        max_rows + 1
    );

    let options = SqliteConnectOptions::new()
        .filename(sqlite_path)
        .read_only(true);
    let mut conn = SqliteConnection::connect_with(&options)
        .await
        .map_err(|e| sqlite_err(&e))?;

    let described = conn
        .describe(&limited_sql)
        .await
        .map_err(|e| sqlite_err(&e))?;
    let columns: Vec<ColumnInfo> = described
        .columns()
        .iter()
        .map(|c| ColumnInfo {
            name: c.name().to_owned(),
            data_type: c.type_info().to_string(),
        })
        .collect();
    let column_count = columns.len();

    let mut query = sqlx::query(&limited_sql);
    for value in &bound {
        query = bind_value(query, value);
    }
    let all_rows: Vec<SqliteRow> = query
        .fetch_all(&mut conn)
        .await
        .map_err(|e| sqlite_err(&e))?;

    let max_rows = usize::try_from(max_rows).unwrap_or(usize::MAX);
    let truncated = all_rows.len() > max_rows;
    let rows = all_rows
        .iter()
        .take(max_rows)
        .map(|row| (0..column_count).map(|i| cell_to_json(row, i)).collect())
        .collect();

    Ok(Executed {
        columns,
        rows,
        truncated,
    })
}

fn bind_value<'q>(
    query: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    value: &'q Value,
) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
    match value {
        Value::Bool(b) => query.bind(*b),
        Value::Number(n) if n.as_i64().is_some() => query.bind(n.as_i64()),
        Value::Number(n) => query.bind(n.as_f64()),
        Value::String(s) => query.bind(s.as_str()),
        Value::Null | Value::Array(_) | Value::Object(_) => query.bind(None::<String>),
    }
}

/// Decode a cell with no compile-time known type: try the storage classes
/// SQLite actually uses (integer, real, text), falling back to null. Each
/// value carries its own storage class regardless of the column's declared
/// type, which is why this tries several decodes rather than branching on
/// column type once.
fn cell_to_json(row: &SqliteRow, i: usize) -> Value {
    if let Ok(v) = row.try_get::<Option<i64>, _>(i) {
        return v.map_or(Value::Null, Value::from);
    }
    if let Ok(Some(v)) = row.try_get::<Option<f64>, _>(i) {
        return Number::from_f64(v).map_or(Value::Null, Value::Number);
    }
    if let Ok(v) = row.try_get::<Option<String>, _>(i) {
        return v.map_or(Value::Null, Value::String);
    }
    Value::Null
}

/// SQLite errors here are always about the query itself (bad SQL, a
/// missing/unbound parameter) -- never a secret worth hiding -- so the
/// message goes straight to the caller.
fn sqlite_err(e: &sqlx::Error) -> AppError {
    AppError::BadRequest(format!("query failed: {e}"))
}
