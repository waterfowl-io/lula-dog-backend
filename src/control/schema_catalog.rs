//! The cached table/column catalog for a workspace's SQLite file, refreshed
//! by introspection (`crate::schema::introspect`) and served at
//! `GET /w/{ws}/schema` so the AI building apps knows what's queryable.

use serde::Serialize;
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use super::ControlPool;
use crate::error::AppResult;

#[derive(Debug, Clone, Serialize, FromRow, ToSchema)]
pub struct SchemaColumn {
    pub table_name: String,
    pub column_name: String,
    pub data_type: String,
    pub is_nullable: bool,
    pub is_primary_key: bool,
    pub foreign_key_table: Option<String>,
    pub foreign_key_column: Option<String>,
}

/// Read the cached catalog without re-introspecting.
pub async fn list(pool: &ControlPool, workspace_id: Uuid) -> AppResult<Vec<SchemaColumn>> {
    Ok(sqlx::query_as::<_, SchemaColumn>(
        "SELECT table_name, column_name, data_type, is_nullable, is_primary_key,
                foreign_key_table, foreign_key_column
         FROM schema_catalog WHERE workspace_id = $1
         ORDER BY table_name, column_name",
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await?)
}

/// Replace the cached catalog for a workspace with a freshly introspected
/// one, in a transaction so readers never see a partially-replaced catalog.
pub async fn replace(
    pool: &ControlPool,
    workspace_id: Uuid,
    columns: &[SchemaColumn],
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM schema_catalog WHERE workspace_id = $1")
        .bind(workspace_id)
        .execute(&mut *tx)
        .await?;
    for column in columns {
        sqlx::query(
            "INSERT INTO schema_catalog
               (workspace_id, table_name, column_name, data_type, is_nullable,
                is_primary_key, foreign_key_table, foreign_key_column)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(workspace_id)
        .bind(&column.table_name)
        .bind(&column.column_name)
        .bind(&column.data_type)
        .bind(column.is_nullable)
        .bind(column.is_primary_key)
        .bind(&column.foreign_key_table)
        .bind(&column.foreign_key_column)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}
