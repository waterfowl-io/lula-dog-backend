//! POST /opportunities/:id/paste: turn a pasted email thread or call notes
//! into a timeline entry, proposed field changes and suggested tasks.
//!
//! Order matters:
//! 1. The raw text is saved as a timeline entry and committed on its own,
//!    before the extractor runs, so nothing is lost if extraction fails.
//! 2. The extractor runs with no transaction open.
//! 3. Its output is written in one transaction. A field that is empty on the
//!    opportunity is filled in and recorded as an accepted change; a field
//!    that already has a different value only gets a proposal. Existing
//!    values are never overwritten here. `last_touch_at` moves forward to the
//!    latest date in the thread (never backwards, never into the future).

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::SqliteConnection;
use utoipa::ToSchema;

use crate::AppState;
use crate::error::{AppError, AppResult};
use crate::extract::{ApiJson, ApiPath};
use crate::extractor::{Extraction, ExtractionInput, Extractor};
use crate::opportunities::{self, Opportunity};
use crate::proposals::{self, Field, FieldChange};
use crate::tasks::{self, Task};
use crate::timeline::{self, TimelineEntry};

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PasteRequest {
    pub text: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PasteResponse {
    pub entry: TimelineEntry,
    /// Changes to fields that already had a value, awaiting accept or reject.
    pub proposals: Vec<FieldChange>,
    /// Empty fields that were filled in, recorded as accepted changes. Also
    /// holds the `last_touch_at` update when the thread moved it forward.
    pub auto_filled: Vec<FieldChange>,
    /// New open tasks, each with `source_entry_id` set to the entry.
    pub suggested_tasks: Vec<Task>,
    /// Set when the extractor failed. The entry is saved regardless; the
    /// other lists are empty.
    pub extraction_error: Option<String>,
}

/// Paste an email thread or notes onto an opportunity.
#[utoipa::path(
    post,
    path = "/opportunities/{id}/paste",
    params(("id" = i64, Path)),
    request_body = PasteRequest,
    responses(
        (status = 201, body = PasteResponse),
        (status = 400, body = crate::error::ErrorBody),
        (status = 404, body = crate::error::ErrorBody),
    ),
    tag = "opportunities"
)]
pub async fn paste(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<i64>,
    ApiJson(req): ApiJson<PasteRequest>,
) -> AppResult<(StatusCode, Json<PasteResponse>)> {
    if req.text.trim().is_empty() {
        return Err(AppError::BadRequest("text cannot be empty".to_owned()));
    }
    let response = run(&state.pool, &state.extractor, id, &req.text).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

pub async fn run(
    pool: &sqlx::SqlitePool,
    extractor: &Arc<dyn Extractor>,
    opportunity_id: i64,
    text: &str,
) -> AppResult<PasteResponse> {
    // 1. Save first, committed on its own.
    let (opportunity, entry) = {
        let mut conn = pool.acquire().await?;
        let opportunity = opportunities::fetch(&mut conn, opportunity_id).await?;
        let entry = timeline::insert(&mut conn, opportunity_id, text).await?;
        (opportunity, entry)
    };

    // 2. Extract, no transaction held.
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let extraction = match extractor
        .extract(ExtractionInput {
            opportunity: &opportunity,
            text,
            today: &today,
        })
        .await
    {
        Ok(extraction) => extraction,
        Err(e) => {
            tracing::warn!(opportunity_id, entry_id = entry.id, error = %e, "extraction failed");
            return Ok(PasteResponse {
                entry,
                proposals: Vec::new(),
                auto_filled: Vec::new(),
                suggested_tasks: Vec::new(),
                extraction_error: Some(e.to_string()),
            });
        }
    };

    // 3. Write the results in one transaction.
    let mut tx = pool.begin().await?;
    let response = write_results(&mut tx, opportunity_id, entry, extraction).await?;
    tx.commit().await?;
    Ok(response)
}

async fn write_results(
    conn: &mut SqliteConnection,
    opportunity_id: i64,
    entry: TimelineEntry,
    extraction: Extraction,
) -> AppResult<PasteResponse> {
    let latest_date = extraction
        .latest_date
        .as_deref()
        .and_then(crate::time::normalize)
        .filter(|d| *d <= crate::time::format(&Utc::now()));
    let entry = timeline::enrich(
        conn,
        entry.id,
        extraction.summary.as_deref().map(str::trim),
        latest_date.as_deref(),
    )
    .await?;

    let mut proposals = Vec::new();
    let mut auto_filled = Vec::new();
    for (field, raw) in [
        (Field::Company, extraction.company),
        (Field::ContactName, extraction.contact_name),
        (Field::ContactEmail, extraction.contact_email),
        (
            Field::ValueCents,
            extraction.value_cents.map(|v| v.to_string()),
        ),
        (Field::Stage, extraction.stage),
        (Field::NextStep, extraction.next_step),
    ] {
        // Untrusted output: anything that does not normalize is dropped.
        let Ok(Some(value)) = field.normalize(raw.as_deref()) else {
            continue;
        };
        // Re-read each time: an earlier auto-fill changed the row.
        let current = opportunities::fetch(conn, opportunity_id).await?;
        if current.get(field).is_none() {
            auto_filled.extend(
                proposals::set_field(conn, &current, field, Some(&value), Some(entry.id)).await?,
            );
        } else {
            proposals
                .extend(proposals::propose(conn, &current, field, &value, Some(entry.id)).await?);
        }
    }

    if let Some(latest) = latest_date {
        let current = opportunities::fetch(conn, opportunity_id).await?;
        if is_newer(&current, &latest) {
            auto_filled.extend(
                proposals::set_field(
                    conn,
                    &current,
                    Field::LastTouchAt,
                    Some(&latest),
                    Some(entry.id),
                )
                .await?,
            );
        }
    }

    let mut suggested_tasks = Vec::new();
    for task in extraction.tasks {
        if task.title.trim().is_empty() {
            continue;
        }
        // An unparseable due date drops the date, not the task.
        let due_at = task.due_at.as_deref().and_then(crate::time::normalize);
        suggested_tasks.push(
            tasks::insert(
                conn,
                opportunity_id,
                &task.title,
                due_at.as_deref(),
                Some(entry.id),
            )
            .await?,
        );
    }

    Ok(PasteResponse {
        entry,
        proposals,
        auto_filled,
        suggested_tasks,
        extraction_error: None,
    })
}

fn is_newer(opportunity: &Opportunity, candidate: &str) -> bool {
    opportunity
        .last_touch_at
        .as_deref()
        .is_none_or(|current| candidate > current)
}
