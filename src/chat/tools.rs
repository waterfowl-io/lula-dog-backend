//! What a model turn can call. Read tools never accept a workspace id --
//! `ToolContext` is already resolved to the caller's own workspace, so
//! there is nothing for the model to point somewhere else. Write tools
//! (data mode) all go through `crate::actions::dispatch`, the same path
//! the UI's own buttons use.
//!
//! A tool's Rust-level error (bad args, not found, a business-rule
//! conflict) is never a hard failure of the chat turn: `execute` turns it
//! into a `{"error": "..."}` tool result the model sees and can react to
//! (ask the user, try something else). Only an infra failure (a database
//! error) is a hard error that aborts the turn -- see `soft_error`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::actions::{self, Undo};
use crate::control::ControlPool;
use crate::control::saved_queries;
use crate::error::{AppError, AppResult};
use crate::opportunities::{self, OpportunityDetail};
use crate::proposals;
use crate::query_runner;
use crate::tasks;
use crate::timeline;

/// Everything a tool call needs, already scoped to one workspace. Deliberately
/// has no field a tool's JSON args could substitute for -- a tool can only
/// ever act within this workspace.
pub struct ToolContext {
    pub pool: SqlitePool,
    pub sqlite_path: PathBuf,
    pub control: Option<ControlPool>,
    pub workspace_id: Uuid,
    pub query_max_rows: i64,
    pub query_timeout_ms: u64,
}

/// A tool definition passed to the provider as part of the request, in
/// OpenAI-compatible function-calling shape.
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
}

pub enum ToolOutcome {
    /// A read, or a write tool call that failed softly (see the module doc
    /// comment) -- either way, just a JSON result to feed back to the model.
    Result(Value),
    /// A write that actually applied. Carries what `crate::chat::handlers`
    /// needs to record it in the action log and emit an `action_applied`
    /// SSE event.
    Write {
        result: Value,
        action: String,
        args: Value,
        undo: Option<Undo>,
    },
}

/// The data-mode tool set: every read tool, plus one write tool per
/// chat-reachable action (see `crate::actions::dispatch`'s doc comment for
/// which `KNOWN_ACTIONS` aren't chat-reachable, and why).
#[expect(
    clippy::too_many_lines,
    reason = "a literal list of tool definitions reads best as one top-to-bottom vec"
)]
pub fn data_mode_tools() -> Vec<ToolSpec> {
    let string_params = |props: Value, required: &[&str]| -> Value {
        json!({ "type": "object", "properties": props, "required": required, "additionalProperties": false })
    };
    vec![
        ToolSpec {
            name: "run_saved_query",
            description: "Run one of this app's saved queries by key and return its rows. \
                Prefer this over read_only_sql when a saved query already answers the question.",
            parameters: string_params(
                json!({
                    "query": {"type": "string", "description": "the saved query's key"},
                    "params": {"type": "object", "description": "named parameters the query expects, if any", "additionalProperties": true},
                }),
                &["query"],
            ),
        },
        ToolSpec {
            name: "read_only_sql",
            description: "Run a single read-only SELECT/WITH statement against this workspace's \
                data and return its rows. Use when no saved query fits. Writes, PRAGMA, ATTACH and \
                multiple statements are all rejected.",
            parameters: string_params(
                json!({"sql": {"type": "string", "description": "a single SELECT or WITH...SELECT statement"}}),
                &["sql"],
            ),
        },
        ToolSpec {
            name: "get_deal",
            description: "Get one deal (opportunity) with its tasks, timeline and pending proposals.",
            parameters: string_params(
                json!({"opportunity_id": {"type": "integer", "description": "the deal's id"}}),
                &["opportunity_id"],
            ),
        },
        ToolSpec {
            name: "get_lead",
            description: "Get one lead candidate with its company and classification.",
            parameters: string_params(
                json!({"lead_id": {"type": "integer", "description": "the lead's id"}}),
                &["lead_id"],
            ),
        },
        ToolSpec {
            name: "opportunity_create",
            description: "Create a new deal (opportunity).",
            parameters: string_params(
                json!({
                    "company": {"type": "string"},
                    "contact_name": {"type": ["string", "null"]},
                    "contact_email": {"type": ["string", "null"]},
                    "value_cents": {"type": ["integer", "null"], "description": "deal value in US cents"},
                    "next_step": {"type": ["string", "null"]},
                }),
                &["company"],
            ),
        },
        ToolSpec {
            name: "opportunity_update",
            description: "Edit a deal's company, contact, value or next step. Omit fields that \
                shouldn't change. Use opportunity_update_stage to change stage instead.",
            parameters: string_params(
                json!({
                    "opportunity_id": {"type": "integer"},
                    "company": {"type": "string"},
                    "contact_name": {"type": ["string", "null"]},
                    "contact_email": {"type": ["string", "null"]},
                    "value_cents": {"type": ["integer", "null"]},
                    "next_step": {"type": ["string", "null"]},
                }),
                &["opportunity_id"],
            ),
        },
        ToolSpec {
            name: "opportunity_update_stage",
            description: "Move a deal to a different pipeline stage.",
            parameters: string_params(
                json!({
                    "opportunity_id": {"type": "integer"},
                    "stage": {"type": "string", "enum": ["lead", "qualified", "proposal", "won", "lost"]},
                }),
                &["opportunity_id", "stage"],
            ),
        },
        ToolSpec {
            name: "task_create",
            description: "Add a task to a deal.",
            parameters: string_params(
                json!({
                    "opportunity_id": {"type": "integer"},
                    "title": {"type": "string"},
                    "due_at": {"type": ["string", "null"], "description": "any common date/time format"},
                }),
                &["opportunity_id", "title"],
            ),
        },
        ToolSpec {
            name: "task_complete",
            description: "Mark a task done or not done.",
            parameters: string_params(
                json!({"task_id": {"type": "integer"}, "done": {"type": "boolean"}}),
                &["task_id", "done"],
            ),
        },
        ToolSpec {
            name: "lead_accept",
            description: "Accept a lead candidate, creating a new deal from it. Not undoable.",
            parameters: string_params(json!({"lead_id": {"type": "integer"}}), &["lead_id"]),
        },
        ToolSpec {
            name: "lead_reject",
            description: "Reject a lead candidate. Not undoable.",
            parameters: string_params(
                json!({"lead_id": {"type": "integer"}, "reason": {"type": "string"}}),
                &["lead_id", "reason"],
            ),
        },
        ToolSpec {
            name: "lead_snooze",
            description: "Snooze a lead candidate until a later date.",
            parameters: string_params(
                json!({"lead_id": {"type": "integer"}, "until": {"type": "string", "description": "any common date/time format"}}),
                &["lead_id", "until"],
            ),
        },
        ToolSpec {
            name: "proposal_accept",
            description: "Accept a pending AI-suggested field change on a deal.",
            parameters: string_params(
                json!({"proposal_id": {"type": "integer"}}),
                &["proposal_id"],
            ),
        },
        ToolSpec {
            name: "proposal_reject",
            description: "Reject a pending AI-suggested field change on a deal.",
            parameters: string_params(
                json!({"proposal_id": {"type": "integer"}}),
                &["proposal_id"],
            ),
        },
    ]
}

/// The `crate::actions::dispatch` action name for a write tool, or `None`
/// for a read tool.
fn action_for_tool(tool_name: &str) -> Option<&'static str> {
    Some(match tool_name {
        "opportunity_create" => "opportunity.create",
        "opportunity_update" => "opportunity.update",
        "opportunity_update_stage" => "opportunity.update_stage",
        "task_create" => "task.create",
        "task_complete" => "task.complete",
        "lead_accept" => "lead.accept",
        "lead_reject" => "lead.reject",
        "lead_snooze" => "lead.snooze",
        "proposal_accept" => "proposal.accept",
        "proposal_reject" => "proposal.reject",
        _ => return None,
    })
}

/// A tool-level ("soft") error becomes a JSON result the model sees; a
/// database/infra error propagates as a hard failure of the whole turn.
/// Shared with `crate::chat::build`'s tool loop.
pub(super) fn soft_error(e: AppError) -> AppResult<Value> {
    match e {
        AppError::Database(_) | AppError::Internal(_) => Err(e),
        other => Ok(json!({ "error": other.to_string() })),
    }
}

pub(super) fn parse_tool_args<T: DeserializeOwned>(raw: &Value) -> AppResult<T> {
    serde_json::from_value(raw.clone())
        .map_err(|e| AppError::BadRequest(format!("invalid tool arguments: {e}")))
}

/// Run one tool call. `write_count`/`max_writes` enforce the per-turn write
/// cap (`CHAT_MAX_WRITES_PER_TURN`): exceeding it is a soft tool error, not
/// a hard stop, so the model can tell the user instead of silently halting.
pub async fn execute(
    ctx: &ToolContext,
    name: &str,
    raw_args: Value,
    write_count: &mut u32,
    max_writes: u32,
) -> AppResult<ToolOutcome> {
    if let Some(action) = action_for_tool(name) {
        if *write_count >= max_writes {
            return Ok(ToolOutcome::Result(json!({
                "error": format!(
                    "write cap reached ({max_writes} writes this turn); ask the user before making more changes"
                )
            })));
        }
        *write_count += 1;
        return Ok(
            match actions::dispatch(&ctx.pool, action, &raw_args).await {
                Ok(outcome) => ToolOutcome::Write {
                    result: outcome.result,
                    action: action.to_owned(),
                    args: raw_args,
                    undo: outcome.undo,
                },
                Err(e) => ToolOutcome::Result(soft_error(e)?),
            },
        );
    }
    let result = match name {
        "run_saved_query" => run_saved_query(ctx, &raw_args).await,
        "read_only_sql" => read_only_sql(ctx, &raw_args).await,
        "get_deal" => get_deal(ctx, &raw_args).await,
        "get_lead" => get_lead(ctx, &raw_args).await,
        other => Err(AppError::BadRequest(format!("unknown tool `{other}`"))),
    };
    match result {
        Ok(v) => Ok(ToolOutcome::Result(v)),
        Err(e) => Ok(ToolOutcome::Result(soft_error(e)?)),
    }
}

/// Rows returned to the model are capped well below the UI's own
/// `QUERY_MAX_ROWS`: a model doesn't need (and shouldn't spend context on)
/// thousands of rows to answer a conversational question.
const MODEL_ROW_CAP: i64 = 50;

#[derive(Deserialize)]
struct RunSavedQueryArgs {
    query: String,
    #[serde(default)]
    params: BTreeMap<String, Value>,
}

async fn run_saved_query(ctx: &ToolContext, raw_args: &Value) -> AppResult<Value> {
    let args: RunSavedQueryArgs = parse_tool_args(raw_args)?;
    let control = ctx
        .control
        .as_ref()
        .ok_or_else(|| AppError::Internal("CONTROL_DATABASE_URL is not configured".to_owned()))?;
    let query = saved_queries::find_by_key(control, ctx.workspace_id, &args.query)
        .await?
        .ok_or_else(|| AppError::BadRequest(format!("no saved query named `{}`", args.query)))?;
    // Saved queries are already admin-authored SELECTs; this is defense in
    // depth, the same check `crate::query_runner::run` applies.
    query_runner::ensure_single_select(&query.sql_text)?;
    let max_rows = ctx.query_max_rows.min(MODEL_ROW_CAP);
    let timeout = Duration::from_millis(ctx.query_timeout_ms);
    let executed = tokio::time::timeout(
        timeout,
        query_runner::execute(&ctx.sqlite_path, &query.sql_text, &args.params, max_rows),
    )
    .await
    .map_err(|_| AppError::BadRequest("query timed out".to_owned()))??;
    Ok(json!({
        "query": args.query,
        "columns": executed.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
        "rows": executed.rows,
        "row_count": executed.rows.len(),
        "truncated": executed.truncated,
    }))
}

#[derive(Deserialize)]
struct ReadOnlySqlArgs {
    sql: String,
}

async fn read_only_sql(ctx: &ToolContext, raw_args: &Value) -> AppResult<Value> {
    let args: ReadOnlySqlArgs = parse_tool_args(raw_args)?;
    query_runner::ensure_single_select(&args.sql)?;
    let max_rows = ctx.query_max_rows.min(MODEL_ROW_CAP);
    let timeout = Duration::from_millis(ctx.query_timeout_ms);
    let executed = tokio::time::timeout(
        timeout,
        query_runner::execute(&ctx.sqlite_path, &args.sql, &BTreeMap::new(), max_rows),
    )
    .await
    .map_err(|_| AppError::BadRequest("query timed out".to_owned()))??;
    Ok(json!({
        "columns": executed.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
        "rows": executed.rows,
        "row_count": executed.rows.len(),
        "truncated": executed.truncated,
    }))
}

#[derive(Deserialize)]
struct DealIdArgs {
    opportunity_id: i64,
}

async fn get_deal(ctx: &ToolContext, raw_args: &Value) -> AppResult<Value> {
    let args: DealIdArgs = parse_tool_args(raw_args)?;
    let mut conn = ctx.pool.acquire().await?;
    let opportunity = opportunities::fetch_view(&mut conn, args.opportunity_id).await?;
    let detail = OpportunityDetail {
        timeline: timeline::list_for(&mut conn, args.opportunity_id).await?,
        pending_proposals: proposals::pending_for(&mut conn, args.opportunity_id).await?,
        tasks: tasks::list_for(&mut conn, args.opportunity_id).await?,
        opportunity,
    };
    serde_json::to_value(detail).map_err(|e| AppError::Internal(format!("serializing deal: {e}")))
}

#[derive(Deserialize)]
struct LeadIdArgs {
    lead_id: i64,
}

async fn get_lead(ctx: &ToolContext, raw_args: &Value) -> AppResult<Value> {
    let args: LeadIdArgs = parse_tool_args(raw_args)?;
    let view = crate::leads::fetch_view(&ctx.pool, args.lead_id).await?;
    serde_json::to_value(view).map_err(|e| AppError::Internal(format!("serializing lead: {e}")))
}
