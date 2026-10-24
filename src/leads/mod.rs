//! Lead candidates: companies that classified above a target profile's
//! threshold, queued for Jack to accept, reject or snooze. Accepting turns
//! one into a step-1 opportunity.

use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqliteConnection};
use utoipa::{IntoParams, ToSchema};

use crate::classifier::Classification;
use crate::companies::{self, Company};
use crate::error::{AppError, AppResult};
use crate::extract::{ApiJson, ApiPath, ApiQuery, IdPath};
use crate::opportunities::{self, NewOpportunity, Stage};
use crate::timeline;
use crate::workspace_db::WorkspaceContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[serde(rename_all = "lowercase")]
#[sqlx(rename_all = "lowercase")]
pub enum LeadStatus {
    New,
    Accepted,
    Rejected,
    Snoozed,
}

impl LeadStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Snoozed => "snoozed",
        }
    }
}

#[derive(Debug, Clone, Serialize, FromRow, ToSchema)]
pub struct LeadCandidate {
    pub id: i64,
    pub company_id: i64,
    pub profile_id: i64,
    pub fit_score: i64,
    pub fit_reason: Option<String>,
    /// A `snoozed` candidate whose `snoozed_until` has passed reads as `new`.
    pub status: LeadStatus,
    pub reject_reason: Option<String>,
    pub snoozed_until: Option<String>,
    pub opportunity_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

/// Queue a classified company as a lead candidate. `None` when the company
/// already has an open (new or snoozed) candidate; never duplicates one.
pub async fn queue_candidate(
    conn: &mut SqliteConnection,
    company_id: i64,
    profile_id: i64,
    fit_score: i64,
    fit_reason: Option<&str>,
) -> AppResult<Option<LeadCandidate>> {
    Ok(sqlx::query_as::<_, LeadCandidate>(
        "INSERT INTO lead_candidates (company_id, profile_id, fit_score, fit_reason)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (company_id) WHERE status IN ('new', 'snoozed') DO NOTHING
         RETURNING *",
    )
    .bind(company_id)
    .bind(profile_id)
    .bind(fit_score)
    .bind(fit_reason)
    .fetch_optional(conn)
    .await?)
}

/// The `status` column, with an expired snooze read as `new`. Used both to
/// filter listings and to atomically claim a candidate for accept/reject/snooze.
const EFFECTIVE_STATUS_SQL: &str = "\
    (CASE WHEN status = 'snoozed' AND snoozed_until IS NOT NULL AND snoozed_until <= datetime('now') \
          THEN 'new' ELSE status END)";

#[derive(Debug, FromRow)]
struct LeadRow {
    #[sqlx(flatten)]
    candidate: LeadCandidate,
    classify_result: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LeadView {
    #[serde(flatten)]
    pub candidate: LeadCandidate,
    pub company: Company,
    pub classification: Option<Classification>,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    /// Defaults to `new` (an expired snooze counts as `new`).
    pub status: Option<LeadStatus>,
    pub min_score: Option<i64>,
    /// Matches the latest classification's `industry`, case-sensitive.
    pub industry: Option<String>,
    pub profile_id: Option<i64>,
    /// Defaults to 50.
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// List leads, highest fit score first.
#[utoipa::path(
    get,
    path = "/w/{workspace_id}/leads",
    params(("workspace_id" = Uuid, Path), ListQuery),
    responses(
        (status = 200, body = [LeadView]),
        (status = 400, body = crate::error::ErrorBody),
    ),
    tag = "leads"
)]
pub async fn list(
    ws: WorkspaceContext,
    ApiQuery(query): ApiQuery<ListQuery>,
) -> AppResult<Json<Vec<LeadView>>> {
    let mut conn = ws.pool.acquire().await?;
    let status = query.status.unwrap_or(LeadStatus::New);
    let sql = format!(
        "SELECT lc.*, {EFFECTIVE_STATUS_SQL} AS status,
                (SELECT result FROM company_enrichments
                  WHERE company_enrichments.company_id = lc.company_id
                    AND company_enrichments.kind = 'classify'
                  ORDER BY company_enrichments.id DESC LIMIT 1) AS classify_result
         FROM lead_candidates lc
         WHERE {EFFECTIVE_STATUS_SQL} = ?1
           AND (?2 IS NULL OR lc.fit_score >= ?2)
           AND (?3 IS NULL OR lc.profile_id = ?3)
           AND (?4 IS NULL OR json_extract((SELECT result FROM company_enrichments
                  WHERE company_enrichments.company_id = lc.company_id
                    AND company_enrichments.kind = 'classify'
                  ORDER BY company_enrichments.id DESC LIMIT 1), '$.industry') = ?4)
         ORDER BY lc.fit_score DESC, lc.id
         LIMIT ?5 OFFSET ?6"
    );
    let rows: Vec<LeadRow> = sqlx::query_as(&sql)
        .bind(status.as_str())
        .bind(query.min_score)
        .bind(query.profile_id)
        .bind(query.industry)
        .bind(query.limit.unwrap_or(50))
        .bind(query.offset.unwrap_or(0))
        .fetch_all(&mut *conn)
        .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let company = companies::fetch(&mut conn, row.candidate.company_id).await?;
        let classification = row
            .classify_result
            .as_deref()
            .and_then(|s| serde_json::from_str::<Classification>(s).ok());
        out.push(LeadView {
            candidate: row.candidate,
            company,
            classification,
        });
    }
    Ok(Json(out))
}

/// Atomically move a candidate out of its open state (`new`, or `snoozed`
/// with an expired `snoozed_until`). `Ok(None)` when no such row exists at
/// all; the caller distinguishes 404 from 409 by re-reading the row.
async fn claim(
    conn: &mut SqliteConnection,
    id: i64,
    set_sql: &str,
) -> AppResult<Option<LeadCandidate>> {
    let sql = format!(
        "UPDATE lead_candidates SET {set_sql}, updated_at = CURRENT_TIMESTAMP
         WHERE id = ?1 AND {EFFECTIVE_STATUS_SQL} = 'new'
         RETURNING *"
    );
    Ok(sqlx::query_as::<_, LeadCandidate>(&sql)
        .bind(id)
        .fetch_optional(conn)
        .await?)
}

async fn claim_or_conflict(
    conn: &mut SqliteConnection,
    id: i64,
    set_sql: &str,
) -> AppResult<LeadCandidate> {
    if let Some(claimed) = claim(conn, id, set_sql).await? {
        return Ok(claimed);
    }
    let existing: Option<LeadStatus> = sqlx::query_scalar(&format!(
        "SELECT {EFFECTIVE_STATUS_SQL} FROM lead_candidates WHERE id = ?1"
    ))
    .bind(id)
    .fetch_optional(conn)
    .await?;
    Err(existing.map_or(AppError::NotFound("lead"), |status| {
        AppError::Conflict(format!("lead {id} is already {}", status.as_str()))
    }))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AcceptResponse {
    pub lead: LeadCandidate,
    pub opportunity_id: i64,
}

/// Accept a lead: creates an opportunity (stage `lead`) linked to the
/// company, with a timeline entry recording the company's website/phone and
/// why it was accepted.
#[utoipa::path(
    post,
    path = "/w/{workspace_id}/leads/{id}/accept",
    params(("workspace_id" = Uuid, Path), ("id" = i64, Path)),
    responses(
        (status = 200, body = AcceptResponse),
        (status = 404, body = crate::error::ErrorBody),
        (status = 409, description = "Already accepted, rejected or no longer open", body = crate::error::ErrorBody),
    ),
    tag = "leads"
)]
pub async fn accept(
    ws: WorkspaceContext,
    ApiPath(IdPath { id }): ApiPath<IdPath>,
) -> AppResult<Json<AcceptResponse>> {
    let mut tx = ws.pool.begin().await?;
    let lead = claim_or_conflict(&mut tx, id, "status = 'accepted'").await?;
    let company = companies::fetch(&mut tx, lead.company_id).await?;

    let opportunity = opportunities::insert(
        &mut tx,
        NewOpportunity {
            company: company.name.clone(),
            contact_name: None,
            contact_email: None,
            value_cents: None,
            stage: Some(Stage::Lead),
            next_step: None,
            last_touch_at: None,
        },
    )
    .await?;

    let note = format!(
        "Accepted from lead gen: {}\nCompany: {}\nWebsite: {}\nPhone: {}",
        lead.fit_reason.as_deref().unwrap_or("no reason recorded"),
        company.name,
        company.website.as_deref().unwrap_or("unknown"),
        company.phone.as_deref().unwrap_or("unknown"),
    );
    timeline::insert(&mut tx, opportunity.id, &note).await?;

    sqlx::query("UPDATE lead_candidates SET opportunity_id = ?1 WHERE id = ?2")
        .bind(opportunity.id)
        .bind(lead.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    Ok(Json(AcceptResponse {
        lead,
        opportunity_id: opportunity.id,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RejectRequest {
    pub reason: String,
}

/// Reject a lead.
#[utoipa::path(
    post,
    path = "/w/{workspace_id}/leads/{id}/reject",
    params(("workspace_id" = Uuid, Path), ("id" = i64, Path)),
    request_body = RejectRequest,
    responses(
        (status = 200, body = LeadCandidate),
        (status = 404, body = crate::error::ErrorBody),
        (status = 409, description = "Already accepted, rejected or no longer open", body = crate::error::ErrorBody),
    ),
    tag = "leads"
)]
pub async fn reject(
    ws: WorkspaceContext,
    ApiPath(IdPath { id }): ApiPath<IdPath>,
    ApiJson(req): ApiJson<RejectRequest>,
) -> AppResult<Json<LeadCandidate>> {
    if req.reason.trim().is_empty() {
        return Err(AppError::BadRequest("reason cannot be empty".to_owned()));
    }
    let mut conn = ws.pool.acquire().await?;
    let sql = format!(
        "UPDATE lead_candidates SET status = 'rejected', reject_reason = ?2, updated_at = CURRENT_TIMESTAMP
         WHERE id = ?1 AND {EFFECTIVE_STATUS_SQL} = 'new'
         RETURNING *"
    );
    let claimed: Option<LeadCandidate> = sqlx::query_as(&sql)
        .bind(id)
        .bind(&req.reason)
        .fetch_optional(&mut *conn)
        .await?;
    if let Some(claimed) = claimed {
        return Ok(Json(claimed));
    }
    let existing: Option<LeadStatus> = sqlx::query_scalar(&format!(
        "SELECT {EFFECTIVE_STATUS_SQL} FROM lead_candidates WHERE id = ?1"
    ))
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    Err(existing.map_or(AppError::NotFound("lead"), |status| {
        AppError::Conflict(format!("lead {id} is already {}", status.as_str()))
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SnoozeRequest {
    /// Any timestamp `time::normalize` accepts.
    pub until: String,
}

/// Snooze a lead until a later date; it reappears as `new` once `until` has
/// passed.
#[utoipa::path(
    post,
    path = "/w/{workspace_id}/leads/{id}/snooze",
    params(("workspace_id" = Uuid, Path), ("id" = i64, Path)),
    request_body = SnoozeRequest,
    responses(
        (status = 200, body = LeadCandidate),
        (status = 400, body = crate::error::ErrorBody),
        (status = 404, body = crate::error::ErrorBody),
        (status = 409, description = "Already accepted, rejected or no longer open", body = crate::error::ErrorBody),
    ),
    tag = "leads"
)]
pub async fn snooze(
    ws: WorkspaceContext,
    ApiPath(IdPath { id }): ApiPath<IdPath>,
    ApiJson(req): ApiJson<SnoozeRequest>,
) -> AppResult<Json<LeadCandidate>> {
    let until = crate::time::normalize(&req.until).ok_or_else(|| {
        AppError::BadRequest(format!(
            "until is not a recognized timestamp: {:?}",
            req.until
        ))
    })?;
    let mut conn = ws.pool.acquire().await?;
    let sql = format!(
        "UPDATE lead_candidates SET status = 'snoozed', snoozed_until = ?2, updated_at = CURRENT_TIMESTAMP
         WHERE id = ?1 AND {EFFECTIVE_STATUS_SQL} = 'new'
         RETURNING *"
    );
    let claimed: Option<LeadCandidate> = sqlx::query_as(&sql)
        .bind(id)
        .bind(&until)
        .fetch_optional(&mut *conn)
        .await?;
    if let Some(claimed) = claimed {
        return Ok(Json(claimed));
    }
    let existing: Option<LeadStatus> = sqlx::query_scalar(&format!(
        "SELECT {EFFECTIVE_STATUS_SQL} FROM lead_candidates WHERE id = ?1"
    ))
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    Err(existing.map_or(AppError::NotFound("lead"), |status| {
        AppError::Conflict(format!("lead {id} is already {}", status.as_str()))
    }))
}
