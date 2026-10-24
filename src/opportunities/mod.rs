//! Opportunities: the deals Jack is working. CRUD, the list with derived
//! open-task counts and the `stale` flag, and the detail view.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Deserializer, Serialize};
use sqlx::{FromRow, SqliteConnection};
use utoipa::{IntoParams, ToSchema};

use crate::AppState;
use crate::error::{AppError, AppResult};
use crate::extract::{ApiJson, ApiPath, ApiQuery};
use crate::proposals::{self, Field, FieldChange};
use crate::tasks::{self, Task};
use crate::timeline::{self, TimelineEntry};

/// Days without a touch after which an open deal is stale.
pub const STALE_AFTER_DAYS: i64 = 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[serde(rename_all = "lowercase")]
#[sqlx(rename_all = "lowercase")]
pub enum Stage {
    Lead,
    Qualified,
    Proposal,
    Won,
    Lost,
}

impl Stage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lead => "lead",
            Self::Qualified => "qualified",
            Self::Proposal => "proposal",
            Self::Won => "won",
            Self::Lost => "lost",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "lead" => Some(Self::Lead),
            "qualified" => Some(Self::Qualified),
            "proposal" => Some(Self::Proposal),
            "won" => Some(Self::Won),
            "lost" => Some(Self::Lost),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, FromRow, ToSchema)]
pub struct Opportunity {
    pub id: i64,
    pub company: String,
    pub contact_name: Option<String>,
    pub contact_email: Option<String>,
    pub value_cents: Option<i64>,
    pub stage: Stage,
    pub next_step: Option<String>,
    pub last_touch_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl Opportunity {
    /// The current value of `field` in its stored text form, as recorded in
    /// `field_changes.old_value`.
    pub fn get(&self, field: Field) -> Option<String> {
        match field {
            Field::Company => Some(self.company.clone()),
            Field::ContactName => self.contact_name.clone(),
            Field::ContactEmail => self.contact_email.clone(),
            Field::ValueCents => self.value_cents.map(|v| v.to_string()),
            Field::Stage => Some(self.stage.as_str().to_owned()),
            Field::NextStep => self.next_step.clone(),
            Field::LastTouchAt => self.last_touch_at.clone(),
        }
    }
}

/// An opportunity plus what is derived from its tasks and touches.
#[derive(Debug, Serialize, FromRow, ToSchema)]
pub struct OpportunityView {
    #[serde(flatten)]
    #[sqlx(flatten)]
    pub opportunity: Opportunity,
    /// Tasks not yet done.
    pub open_task_count: i64,
    /// Open tasks whose due date is before today (UTC).
    pub overdue_task_count: i64,
    /// An open deal (not won or lost) whose last touch, or creation if never
    /// touched, is more than 14 days ago, or that has an overdue open task.
    pub stale: bool,
}

/// `OpportunityView` rows. Overdue compares calendar days, so a task due
/// today is not overdue until tomorrow.
const VIEW_SQL: &str = "
SELECT v.*,
       (v.stage NOT IN ('won', 'lost')
        AND (COALESCE(v.last_touch_at, v.created_at) < datetime('now', '-14 days')
             OR v.overdue_task_count > 0)) AS stale
FROM (
  SELECT o.*,
         (SELECT COUNT(*) FROM tasks t
           WHERE t.opportunity_id = o.id AND t.done = 0) AS open_task_count,
         (SELECT COUNT(*) FROM tasks t
           WHERE t.opportunity_id = o.id AND t.done = 0
             AND t.due_at IS NOT NULL AND date(t.due_at) < date('now')) AS overdue_task_count
  FROM opportunities o
) v";

pub async fn fetch(conn: &mut SqliteConnection, id: i64) -> AppResult<Opportunity> {
    sqlx::query_as::<_, Opportunity>("SELECT * FROM opportunities WHERE id = ?1")
        .bind(id)
        .fetch_optional(conn)
        .await?
        .ok_or(AppError::NotFound("opportunity"))
}

pub async fn fetch_view(conn: &mut SqliteConnection, id: i64) -> AppResult<OpportunityView> {
    sqlx::query_as::<_, OpportunityView>(&format!("SELECT * FROM ({VIEW_SQL}) WHERE id = ?1"))
        .bind(id)
        .fetch_optional(conn)
        .await?
        .ok_or(AppError::NotFound("opportunity"))
}

pub async fn list_views(
    conn: &mut SqliteConnection,
    stage: Option<Stage>,
    stale_only: bool,
) -> AppResult<Vec<OpportunityView>> {
    // Stale alerts lead with the deal that has gone longest without a touch.
    let order = if stale_only {
        "COALESCE(last_touch_at, created_at) ASC, id ASC"
    } else {
        "updated_at DESC, id DESC"
    };
    let sql = format!(
        "SELECT * FROM ({VIEW_SQL})
         WHERE (?1 IS NULL OR stage = ?1) AND (?2 = 0 OR stale = 1)
         ORDER BY {order}"
    );
    Ok(sqlx::query_as::<_, OpportunityView>(&sql)
        .bind(stage)
        .bind(stale_only)
        .fetch_all(conn)
        .await?)
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    /// Only opportunities in this stage.
    pub stage: Option<Stage>,
}

/// List opportunities, newest activity first.
#[utoipa::path(
    get,
    path = "/opportunities",
    params(ListQuery),
    responses(
        (status = 200, body = [OpportunityView]),
        (status = 400, body = crate::error::ErrorBody),
    ),
    tag = "opportunities"
)]
pub async fn list(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<ListQuery>,
) -> AppResult<Json<Vec<OpportunityView>>> {
    let mut conn = state.pool.acquire().await?;
    Ok(Json(list_views(&mut conn, query.stage, false).await?))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewOpportunity {
    pub company: String,
    pub contact_name: Option<String>,
    pub contact_email: Option<String>,
    pub value_cents: Option<i64>,
    /// Defaults to `lead`.
    pub stage: Option<Stage>,
    pub next_step: Option<String>,
    /// Any timestamp `time::normalize` accepts; stored as `YYYY-MM-DD HH:MM:SS` UTC.
    pub last_touch_at: Option<String>,
}

/// Validate and insert. The one writer of new opportunity rows; the seed
/// command uses it too.
pub async fn insert(conn: &mut SqliteConnection, new: NewOpportunity) -> AppResult<Opportunity> {
    let company = norm(Field::Company, Some(&new.company))?;
    let contact_name = norm(Field::ContactName, new.contact_name.as_deref())?;
    let contact_email = norm(Field::ContactEmail, new.contact_email.as_deref())?;
    // Normalized means it parses.
    let value_cents = norm(
        Field::ValueCents,
        new.value_cents.map(|v| v.to_string()).as_deref(),
    )?
    .and_then(|v| v.parse::<i64>().ok());
    let next_step = norm(Field::NextStep, new.next_step.as_deref())?;
    let last_touch_at = norm(Field::LastTouchAt, new.last_touch_at.as_deref())?;
    let stage = new.stage.unwrap_or(Stage::Lead);
    Ok(sqlx::query_as::<_, Opportunity>(
        "INSERT INTO opportunities
           (company, contact_name, contact_email, value_cents, stage, next_step, last_touch_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         RETURNING *",
    )
    .bind(company)
    .bind(contact_name)
    .bind(contact_email)
    .bind(value_cents)
    .bind(stage)
    .bind(next_step)
    .bind(last_touch_at)
    .fetch_one(conn)
    .await?)
}

fn norm(field: Field, value: Option<&str>) -> AppResult<Option<String>> {
    field.normalize(value).map_err(AppError::BadRequest)
}

/// Create an opportunity.
#[utoipa::path(
    post,
    path = "/opportunities",
    request_body = NewOpportunity,
    responses(
        (status = 201, body = OpportunityView),
        (status = 400, body = crate::error::ErrorBody),
    ),
    tag = "opportunities"
)]
pub async fn create(
    State(state): State<AppState>,
    ApiJson(new): ApiJson<NewOpportunity>,
) -> AppResult<(StatusCode, Json<OpportunityView>)> {
    let mut conn = state.pool.acquire().await?;
    let created = insert(&mut conn, new).await?;
    Ok((
        StatusCode::CREATED,
        Json(fetch_view(&mut conn, created.id).await?),
    ))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct OpportunityDetail {
    pub opportunity: OpportunityView,
    /// Newest first.
    pub timeline: Vec<TimelineEntry>,
    /// Proposed field changes awaiting accept or reject, oldest first.
    pub pending_proposals: Vec<FieldChange>,
    /// Open tasks first, then by due date.
    pub tasks: Vec<Task>,
}

/// One opportunity with its timeline, pending proposals and tasks.
#[utoipa::path(
    get,
    path = "/opportunities/{id}",
    params(("id" = i64, Path)),
    responses(
        (status = 200, body = OpportunityDetail),
        (status = 404, body = crate::error::ErrorBody),
    ),
    tag = "opportunities"
)]
pub async fn get(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<i64>,
) -> AppResult<Json<OpportunityDetail>> {
    let mut conn = state.pool.acquire().await?;
    let opportunity = fetch_view(&mut conn, id).await?;
    Ok(Json(OpportunityDetail {
        opportunity,
        timeline: timeline::list_for(&mut conn, id).await?,
        pending_proposals: proposals::pending_for(&mut conn, id).await?,
        tasks: tasks::list_for(&mut conn, id).await?,
    }))
}

/// A partial update. An absent field is left alone; `null` clears a nullable
/// field and is rejected for `company` and `stage`.
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpportunityPatch {
    #[serde(default, deserialize_with = "not_null")]
    pub company: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub contact_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub contact_email: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<i64>)]
    pub value_cents: Option<Option<i64>>,
    #[serde(default, deserialize_with = "not_null")]
    pub stage: Option<Stage>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub next_step: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub last_touch_at: Option<Option<String>>,
}

/// Distinguishes `"field": null` (`Some(None)`) from an absent field (`None`,
/// via `#[serde(default)]`).
pub fn nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// For a patch field that cannot be cleared: absent is `None` (via
/// `#[serde(default)]`), and an explicit `null` is a 400 instead of quietly
/// reading as absent.
pub fn not_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

impl OpportunityPatch {
    fn changes(self) -> Vec<(Field, Option<String>)> {
        let mut out = Vec::new();
        if let Some(v) = self.company {
            out.push((Field::Company, Some(v)));
        }
        if let Some(v) = self.contact_name {
            out.push((Field::ContactName, v));
        }
        if let Some(v) = self.contact_email {
            out.push((Field::ContactEmail, v));
        }
        if let Some(v) = self.value_cents {
            out.push((Field::ValueCents, v.map(|c| c.to_string())));
        }
        if let Some(v) = self.stage {
            out.push((Field::Stage, Some(v.as_str().to_owned())));
        }
        if let Some(v) = self.next_step {
            out.push((Field::NextStep, v));
        }
        if let Some(v) = self.last_touch_at {
            out.push((Field::LastTouchAt, v));
        }
        out
    }
}

/// Edit fields directly. Each changed field is recorded in the audit trail
/// as an accepted change with no source entry.
#[utoipa::path(
    patch,
    path = "/opportunities/{id}",
    params(("id" = i64, Path)),
    request_body = OpportunityPatch,
    responses(
        (status = 200, body = OpportunityView),
        (status = 400, body = crate::error::ErrorBody),
        (status = 404, body = crate::error::ErrorBody),
    ),
    tag = "opportunities"
)]
pub async fn patch(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<i64>,
    ApiJson(patch): ApiJson<OpportunityPatch>,
) -> AppResult<Json<OpportunityView>> {
    let mut tx = state.pool.begin().await?;
    let mut current = fetch(&mut tx, id).await?;
    for (field, value) in patch.changes() {
        let value = norm(field, value.as_deref())?;
        proposals::set_field(&mut tx, &current, field, value.as_deref(), None).await?;
        current = fetch(&mut tx, id).await?;
    }
    let view = fetch_view(&mut tx, id).await?;
    tx.commit().await?;
    Ok(Json(view))
}
