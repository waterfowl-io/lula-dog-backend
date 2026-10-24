//! Timeline entries: the append-only record of what was pasted in. The schema
//! enforces it with triggers; this module only ever inserts, and fills the
//! write-once enrichment columns.

use serde::Serialize;
use sqlx::{FromRow, SqliteConnection};
use utoipa::ToSchema;

use crate::error::AppResult;

#[derive(Debug, Clone, Serialize, FromRow, ToSchema)]
pub struct TimelineEntry {
    pub id: i64,
    pub opportunity_id: i64,
    /// Exactly what was pasted.
    pub raw_text: String,
    /// One-line summary from the extractor.
    pub summary: Option<String>,
    /// The latest date found in the text, `YYYY-MM-DD HH:MM:SS` UTC.
    pub occurred_at: Option<String>,
    pub created_at: String,
}

pub async fn insert(
    conn: &mut SqliteConnection,
    opportunity_id: i64,
    raw_text: &str,
) -> AppResult<TimelineEntry> {
    Ok(sqlx::query_as::<_, TimelineEntry>(
        "INSERT INTO timeline_entries (opportunity_id, raw_text) VALUES (?1, ?2) RETURNING *",
    )
    .bind(opportunity_id)
    .bind(raw_text)
    .fetch_one(conn)
    .await?)
}

/// Fill the enrichment columns. Only NULL columns are written (the trigger
/// would refuse anything else), so a second call is a no-op.
pub async fn enrich(
    conn: &mut SqliteConnection,
    id: i64,
    summary: Option<&str>,
    occurred_at: Option<&str>,
) -> AppResult<TimelineEntry> {
    Ok(sqlx::query_as::<_, TimelineEntry>(
        "UPDATE timeline_entries
         SET summary = COALESCE(summary, ?1), occurred_at = COALESCE(occurred_at, ?2)
         WHERE id = ?3
         RETURNING *",
    )
    .bind(summary)
    .bind(occurred_at)
    .bind(id)
    .fetch_one(conn)
    .await?)
}

pub async fn list_for(
    conn: &mut SqliteConnection,
    opportunity_id: i64,
) -> AppResult<Vec<TimelineEntry>> {
    Ok(sqlx::query_as::<_, TimelineEntry>(
        "SELECT * FROM timeline_entries WHERE opportunity_id = ?1
         ORDER BY COALESCE(occurred_at, created_at) DESC, id DESC",
    )
    .bind(opportunity_id)
    .fetch_all(conn)
    .await?)
}
