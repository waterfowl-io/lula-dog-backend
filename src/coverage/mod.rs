//! `coverage_log`: what has already been imported or searched, with counts.
//! Importers write one row per file processed; the Places search writes one
//! row per batch. Idempotency itself lives in `company_sources` (imports)
//! and `company_enrichments` (per-company enrichment selection); this table
//! is the human-visible record of what a run covered and how much of it was
//! new.

use serde::Serialize;
use sqlx::{FromRow, SqliteConnection};

use crate::error::AppResult;

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct CoverageEntry {
    pub id: i64,
    pub source: String,
    pub query: Option<String>,
    pub region: Option<String>,
    pub category: Option<String>,
    pub period: Option<String>,
    pub run_at: String,
    pub seen_count: i64,
    pub new_count: i64,
    pub duplicate_count: i64,
    pub filtered_count: i64,
    pub error_count: i64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Counts {
    pub seen: i64,
    pub new: i64,
    pub duplicate: i64,
    pub filtered: i64,
    pub error: i64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NewCoverage<'a> {
    pub source: &'a str,
    pub query: Option<&'a str>,
    pub region: Option<&'a str>,
    pub category: Option<&'a str>,
    pub period: Option<&'a str>,
}

pub async fn record(
    conn: &mut SqliteConnection,
    new: NewCoverage<'_>,
    counts: Counts,
    error: Option<&str>,
) -> AppResult<CoverageEntry> {
    Ok(sqlx::query_as::<_, CoverageEntry>(
        "INSERT INTO coverage_log
           (source, query, region, category, period,
            seen_count, new_count, duplicate_count, filtered_count, error_count, error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         RETURNING *",
    )
    .bind(new.source)
    .bind(new.query)
    .bind(new.region)
    .bind(new.category)
    .bind(new.period)
    .bind(counts.seen)
    .bind(counts.new)
    .bind(counts.duplicate)
    .bind(counts.filtered)
    .bind(counts.error)
    .bind(error)
    .fetch_one(conn)
    .await?)
}

/// Whether `source` has already recorded a run for `period` (e.g. an
/// imported file name). Lets a caller skip whole-file reprocessing when that
/// is cheaper than relying on per-row idempotency.
pub async fn already_covered(
    conn: &mut SqliteConnection,
    source: &str,
    period: &str,
) -> AppResult<bool> {
    let id: Option<i64> =
        sqlx::query_scalar("SELECT id FROM coverage_log WHERE source = ?1 AND period = ?2")
            .bind(source)
            .bind(period)
            .fetch_optional(conn)
            .await?;
    Ok(id.is_some())
}
