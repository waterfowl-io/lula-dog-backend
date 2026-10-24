//! `GET /w/{workspace_id}/schema`: introspect the workspace's SQLite file
//! (tables, columns, types, nullability, primary/foreign keys) and refresh
//! the cached `schema_catalog` in the control plane with the result, so the
//! AI building apps knows what's queryable without a live connection.

use std::collections::HashMap;

use axum::Json;
use axum::extract::State;
use sqlx::{FromRow, SqlitePool};

use crate::AppState;
use crate::control::schema_catalog::{self, SchemaColumn};
use crate::error::AppResult;
use crate::workspace_db::WorkspaceContext;

#[derive(FromRow)]
struct TableInfoRow {
    name: String,
    #[sqlx(rename = "type")]
    sql_type: String,
    notnull: i64,
    pk: i64,
}

#[derive(FromRow)]
struct ForeignKeyRow {
    table: String,
    from: String,
    to: String,
}

/// `PRAGMA table_info`/`foreign_key_list` for every user table. Table names
/// come from `sqlite_master`, not request input, so formatting them
/// directly into the `PRAGMA` string (which can't take bound params) is
/// safe.
pub async fn introspect(pool: &SqlitePool) -> AppResult<Vec<SchemaColumn>> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name != '_sqlx_migrations'
         ORDER BY name",
    )
    .fetch_all(pool)
    .await?;

    let mut columns = Vec::new();
    for table in tables {
        let foreign_keys: Vec<ForeignKeyRow> =
            sqlx::query_as(&format!("PRAGMA foreign_key_list({table})"))
                .fetch_all(pool)
                .await?;
        let fk_by_column: HashMap<String, (String, String)> = foreign_keys
            .into_iter()
            .map(|fk| (fk.from, (fk.table, fk.to)))
            .collect();

        let table_info: Vec<TableInfoRow> = sqlx::query_as(&format!("PRAGMA table_info({table})"))
            .fetch_all(pool)
            .await?;
        for column in table_info {
            let foreign_key = fk_by_column.get(&column.name);
            columns.push(SchemaColumn {
                table_name: table.clone(),
                column_name: column.name,
                data_type: column.sql_type,
                is_nullable: column.notnull == 0,
                is_primary_key: column.pk != 0,
                foreign_key_table: foreign_key.map(|(t, _)| t.clone()),
                foreign_key_column: foreign_key.map(|(_, c)| c.clone()),
            });
        }
    }
    Ok(columns)
}

/// Introspect the workspace's SQLite file and refresh the cached catalog.
#[utoipa::path(
    get,
    path = "/w/{workspace_id}/schema",
    params(("workspace_id" = uuid::Uuid, Path)),
    responses((status = 200, body = [SchemaColumn])),
    tag = "schema"
)]
pub async fn get_catalog(
    State(state): State<AppState>,
    ws: WorkspaceContext,
) -> AppResult<Json<Vec<SchemaColumn>>> {
    let columns = introspect(&ws.pool).await?;
    if let Some(control) = &state.control {
        schema_catalog::replace(control, ws.id, &columns).await?;
    }
    Ok(Json(columns))
}
