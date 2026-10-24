//! `field_changes`: AI proposals and the audit trail of every applied change.
//!
//! This module is the one writer of opportunity field values after creation.
//! A direct PATCH, an auto-fill from a paste, and an accepted proposal all go
//! through `apply` and leave a `field_changes` row behind.

use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqliteConnection};
use utoipa::ToSchema;

use crate::error::{AppError, AppResult};
use crate::extract::{ApiPath, IdPath};
use crate::opportunities::{self, Opportunity, OpportunityView, Stage};
use crate::workspace_db::WorkspaceContext;

/// An opportunity field that a change can target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum Field {
    Company,
    ContactName,
    ContactEmail,
    ValueCents,
    Stage,
    NextStep,
    LastTouchAt,
}

impl Field {
    /// The `opportunities` column. A fixed string per variant, so it is safe
    /// to interpolate into SQL.
    const fn column(self) -> &'static str {
        match self {
            Self::Company => "company",
            Self::ContactName => "contact_name",
            Self::ContactEmail => "contact_email",
            Self::ValueCents => "value_cents",
            Self::Stage => "stage",
            Self::NextStep => "next_step",
            Self::LastTouchAt => "last_touch_at",
        }
    }

    /// Validate a value for this field and return its stored text form.
    /// Blank text is `None`. `Err` carries a message for the caller.
    pub fn normalize(self, value: Option<&str>) -> Result<Option<String>, String> {
        let value = value.map(str::trim).filter(|v| !v.is_empty());
        let name = self.column();
        match (self, value) {
            (Self::Company | Self::Stage, None) => Err(format!("{name} cannot be empty")),
            (_, None) => Ok(None),
            (Self::Company | Self::ContactName | Self::NextStep, Some(v)) => Ok(Some(v.to_owned())),
            (Self::ContactEmail, Some(v)) => {
                let valid = !v.contains(char::is_whitespace)
                    && v.split_once('@')
                        .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.'));
                if valid {
                    Ok(Some(v.to_owned()))
                } else {
                    Err(format!("{name} is not an email address: {v:?}"))
                }
            }
            (Self::ValueCents, Some(v)) => match v.parse::<i64>() {
                Ok(c) if c >= 0 => Ok(Some(c.to_string())),
                _ => Err(format!("{name} must be a non-negative integer, got {v:?}")),
            },
            (Self::Stage, Some(v)) => Stage::parse(v)
                .map(|s| Some(s.as_str().to_owned()))
                .ok_or_else(|| {
                    format!("{name} must be one of lead, qualified, proposal, won, lost, got {v:?}")
                }),
            (Self::LastTouchAt, Some(v)) => crate::time::normalize(v)
                .map(Some)
                .ok_or_else(|| format!("{name} is not a recognized timestamp: {v:?}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[serde(rename_all = "lowercase")]
#[sqlx(rename_all = "lowercase")]
pub enum ChangeStatus {
    Proposed,
    Accepted,
    Rejected,
}

impl ChangeStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Debug, Clone, Serialize, FromRow, ToSchema)]
pub struct FieldChange {
    pub id: i64,
    pub opportunity_id: i64,
    /// The timeline entry the change came from; `null` for a direct edit.
    pub entry_id: Option<i64>,
    pub field: Field,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    pub status: ChangeStatus,
    pub created_at: String,
}

async fn record(
    conn: &mut SqliteConnection,
    opportunity: &Opportunity,
    field: Field,
    new_value: Option<&str>,
    entry_id: Option<i64>,
    status: ChangeStatus,
) -> AppResult<FieldChange> {
    Ok(sqlx::query_as::<_, FieldChange>(
        "INSERT INTO field_changes
           (opportunity_id, entry_id, field, old_value, new_value, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         RETURNING *",
    )
    .bind(opportunity.id)
    .bind(entry_id)
    .bind(field)
    .bind(opportunity.get(field))
    .bind(new_value)
    .bind(status)
    .fetch_one(conn)
    .await?)
}

/// Write a normalized value to the opportunity row.
async fn apply(
    conn: &mut SqliteConnection,
    opportunity_id: i64,
    field: Field,
    value: Option<&str>,
) -> AppResult<()> {
    let sql = format!(
        "UPDATE opportunities SET {} = ?1, updated_at = CURRENT_TIMESTAMP WHERE id = ?2",
        field.column()
    );
    let query = sqlx::query(&sql);
    let query = if field == Field::ValueCents {
        query.bind(value.and_then(|v| v.parse::<i64>().ok()))
    } else {
        query.bind(value)
    };
    query.bind(opportunity_id).execute(conn).await?;
    Ok(())
}

/// Apply a change now and record it as accepted. `value` must already be
/// normalized. Returns `None` when the field already holds `value`.
pub async fn set_field(
    conn: &mut SqliteConnection,
    opportunity: &Opportunity,
    field: Field,
    value: Option<&str>,
    entry_id: Option<i64>,
) -> AppResult<Option<FieldChange>> {
    if opportunity.get(field).as_deref() == value {
        return Ok(None);
    }
    apply(conn, opportunity.id, field, value).await?;
    let change = record(
        conn,
        opportunity,
        field,
        value,
        entry_id,
        ChangeStatus::Accepted,
    )
    .await?;
    Ok(Some(change))
}

/// Record a proposed change without touching the opportunity. `value` must
/// already be normalized. Returns `None` when the field already holds
/// `value` or the same change is already pending.
pub async fn propose(
    conn: &mut SqliteConnection,
    opportunity: &Opportunity,
    field: Field,
    value: &str,
    entry_id: Option<i64>,
) -> AppResult<Option<FieldChange>> {
    if opportunity.get(field).as_deref() == Some(value) {
        return Ok(None);
    }
    let duplicate: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM field_changes
         WHERE opportunity_id = ?1 AND field = ?2 AND new_value = ?3 AND status = 'proposed'",
    )
    .bind(opportunity.id)
    .bind(field)
    .bind(value)
    .fetch_optional(&mut *conn)
    .await?;
    if duplicate.is_some() {
        return Ok(None);
    }
    let change = record(
        conn,
        opportunity,
        field,
        Some(value),
        entry_id,
        ChangeStatus::Proposed,
    )
    .await?;
    Ok(Some(change))
}

pub async fn pending_for(
    conn: &mut SqliteConnection,
    opportunity_id: i64,
) -> AppResult<Vec<FieldChange>> {
    Ok(sqlx::query_as::<_, FieldChange>(
        "SELECT * FROM field_changes
         WHERE opportunity_id = ?1 AND status = 'proposed'
         ORDER BY id",
    )
    .bind(opportunity_id)
    .fetch_all(conn)
    .await?)
}

/// Move a proposal out of `proposed`. The `WHERE status = 'proposed'` makes
/// the claim atomic: of two concurrent accepts, one wins and one gets 409.
async fn resolve(conn: &mut SqliteConnection, id: i64, to: ChangeStatus) -> AppResult<FieldChange> {
    let claimed = sqlx::query_as::<_, FieldChange>(
        "UPDATE field_changes SET status = ?1
         WHERE id = ?2 AND status = 'proposed'
         RETURNING *",
    )
    .bind(to)
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(change) = claimed {
        return Ok(change);
    }
    let existing: Option<ChangeStatus> =
        sqlx::query_scalar("SELECT status FROM field_changes WHERE id = ?1")
            .bind(id)
            .fetch_optional(conn)
            .await?;
    Err(existing.map_or(AppError::NotFound("proposal"), |status| {
        AppError::Conflict(format!("proposal {id} is already {}", status.as_str()))
    }))
}

async fn accept_one(conn: &mut SqliteConnection, id: i64) -> AppResult<FieldChange> {
    let change = resolve(conn, id, ChangeStatus::Accepted).await?;
    // Re-validated on the way in, in case the row was written by something
    // other than `propose`.
    let value = change
        .field
        .normalize(change.new_value.as_deref())
        .map_err(AppError::BadRequest)?;
    apply(conn, change.opportunity_id, change.field, value.as_deref()).await?;
    Ok(change)
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AcceptResponse {
    pub proposal: FieldChange,
    pub opportunity: OpportunityView,
}

/// Accept a proposal and apply its value to the opportunity.
#[utoipa::path(
    post,
    path = "/w/{workspace_id}/proposals/{id}/accept",
    params(("workspace_id" = Uuid, Path), ("id" = i64, Path)),
    responses(
        (status = 200, body = AcceptResponse),
        (status = 404, body = crate::error::ErrorBody),
        (status = 409, description = "Already accepted or rejected", body = crate::error::ErrorBody),
    ),
    tag = "proposals"
)]
pub async fn accept(
    ws: WorkspaceContext,
    ApiPath(IdPath { id }): ApiPath<IdPath>,
) -> AppResult<Json<AcceptResponse>> {
    let mut tx = ws.pool.begin().await?;
    let proposal = accept_one(&mut tx, id).await?;
    let opportunity = opportunities::fetch_view(&mut tx, proposal.opportunity_id).await?;
    tx.commit().await?;
    Ok(Json(AcceptResponse {
        proposal,
        opportunity,
    }))
}

/// Reject a proposal. The opportunity is unchanged.
#[utoipa::path(
    post,
    path = "/w/{workspace_id}/proposals/{id}/reject",
    params(("workspace_id" = Uuid, Path), ("id" = i64, Path)),
    responses(
        (status = 200, body = FieldChange),
        (status = 404, body = crate::error::ErrorBody),
        (status = 409, description = "Already accepted or rejected", body = crate::error::ErrorBody),
    ),
    tag = "proposals"
)]
pub async fn reject(
    ws: WorkspaceContext,
    ApiPath(IdPath { id }): ApiPath<IdPath>,
) -> AppResult<Json<FieldChange>> {
    let mut conn = ws.pool.acquire().await?;
    Ok(Json(resolve(&mut conn, id, ChangeStatus::Rejected).await?))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AcceptAllResponse {
    /// Accepted in creation order, so for two proposals on one field the
    /// newer one wins.
    pub accepted: Vec<FieldChange>,
    pub opportunity: OpportunityView,
}

/// Accept every pending proposal on an opportunity.
#[utoipa::path(
    post,
    path = "/w/{workspace_id}/opportunities/{id}/proposals/accept-all",
    params(("workspace_id" = Uuid, Path), ("id" = i64, Path)),
    responses(
        (status = 200, body = AcceptAllResponse),
        (status = 404, body = crate::error::ErrorBody),
    ),
    tag = "proposals"
)]
pub async fn accept_all(
    ws: WorkspaceContext,
    ApiPath(IdPath { id }): ApiPath<IdPath>,
) -> AppResult<Json<AcceptAllResponse>> {
    let mut tx = ws.pool.begin().await?;
    opportunities::fetch(&mut tx, id).await?;
    let mut accepted = Vec::new();
    for pending in pending_for(&mut tx, id).await? {
        accepted.push(accept_one(&mut tx, pending.id).await?);
    }
    let opportunity = opportunities::fetch_view(&mut tx, id).await?;
    tx.commit().await?;
    Ok(Json(AcceptAllResponse {
        accepted,
        opportunity,
    }))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DismissAllResponse {
    pub rejected: Vec<FieldChange>,
}

/// Reject every pending proposal on an opportunity.
#[utoipa::path(
    post,
    path = "/w/{workspace_id}/opportunities/{id}/proposals/dismiss-all",
    params(("workspace_id" = Uuid, Path), ("id" = i64, Path)),
    responses(
        (status = 200, body = DismissAllResponse),
        (status = 404, body = crate::error::ErrorBody),
    ),
    tag = "proposals"
)]
pub async fn dismiss_all(
    ws: WorkspaceContext,
    ApiPath(IdPath { id }): ApiPath<IdPath>,
) -> AppResult<Json<DismissAllResponse>> {
    let mut tx = ws.pool.begin().await?;
    opportunities::fetch(&mut tx, id).await?;
    let rejected = sqlx::query_as::<_, FieldChange>(
        "UPDATE field_changes SET status = 'rejected'
         WHERE opportunity_id = ?1 AND status = 'proposed'
         RETURNING *",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(DismissAllResponse { rejected }))
}
