//! The action-dispatch layer: a named action (one of
//! `crate::app_definition::KNOWN_ACTIONS`) plus JSON args, applied through
//! the exact same `*_via_pool` functions the HTTP handlers call.
//!
//! This exists so chat's data-mode write tools (`crate::chat::tools`) and a
//! person clicking a button in the UI can never validate or authorize
//! differently: `dispatch` is the one place a name turns into a write, and
//! it never reads a workspace id out of `args` -- the caller always passes
//! `pool`, already resolved to the request's own workspace.
//!
//! Undo is tracked here too (`Undo`, `undo`): most of this reuses existing
//! reversal logic one-for-one -- a direct field edit records exactly the
//! same `field_changes` row an accepted proposal does, so undoing it is
//! `proposals::unaccept_via_pool`; accepting/rejecting a proposal is undone
//! by the HTTP `unaccept`/`unreject` endpoints' own logic. Only task
//! creation, task completion and lead snoozing needed new reversal
//! functions, since nothing else in the app previously needed to undo them.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, to_value};
use sqlx::SqlitePool;

use crate::error::{AppError, AppResult};
use crate::leads::{self, LeadStatus};
use crate::opportunities::{self, NewOpportunity, OpportunityPatch};
use crate::proposals;
use crate::tasks::{self, NewTask, TaskPatch};

/// What's needed to reverse an applied action, stored as JSON on
/// `chat_actions.undo` and replayed by `undo`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Undo {
    /// One or more `field_changes` rows a direct `opportunity.update` /
    /// `opportunity.update_stage` call recorded as accepted; reversed the
    /// same way an accepted proposal is.
    OpportunityFields {
        change_ids: Vec<i64>,
    },
    DeleteTask {
        task_id: i64,
    },
    SetTaskDone {
        task_id: i64,
        done: bool,
    },
    RestoreLead {
        lead_id: i64,
        status: LeadStatus,
        reject_reason: Option<String>,
        snoozed_until: Option<String>,
    },
    UnacceptProposal {
        proposal_id: i64,
    },
    UnrejectProposal {
        proposal_id: i64,
    },
}

pub struct ActionOutcome {
    pub result: Value,
    pub undo: Option<Undo>,
}

impl ActionOutcome {
    const fn new(result: Value) -> Self {
        Self { result, undo: None }
    }

    const fn with_undo(result: Value, undo: Undo) -> Self {
        Self {
            result,
            undo: Some(undo),
        }
    }
}

fn parse_args<T: DeserializeOwned>(action: &str, raw: &Value) -> AppResult<T> {
    serde_json::from_value(raw.clone())
        .map_err(|e| AppError::BadRequest(format!("invalid args for `{action}`: {e}")))
}

fn to_json<T: Serialize>(value: &T) -> AppResult<Value> {
    to_value(value).map_err(|e| AppError::Internal(format!("serializing action result: {e}")))
}

/// Pull an integer id named `id_field` out of `raw` (an object), returning
/// it plus the remaining fields as their own JSON object -- so a single
/// typed struct (e.g. `OpportunityPatch`, `NewTask`) can `deny_unknown_fields`
/// on the rest without also needing to know the id field's name.
fn split_id(action: &str, raw: &Value, id_field: &str) -> AppResult<(i64, Value)> {
    let obj = raw.as_object().ok_or_else(|| {
        AppError::BadRequest(format!("args for `{action}` must be a JSON object"))
    })?;
    let id = obj.get(id_field).and_then(Value::as_i64).ok_or_else(|| {
        AppError::BadRequest(format!("args for `{action}` need an integer `{id_field}`"))
    })?;
    let mut rest = obj.clone();
    rest.remove(id_field);
    Ok((id, Value::Object(rest)))
}

/// Apply a named action with its JSON args, exactly as the matching HTTP
/// endpoint would. Actions reachable only via a dedicated undo flow
/// (`proposal.unaccept`/`proposal.unreject`) and `paste.submit` (which needs
/// an `Extractor`, not just a pool) are not dispatchable here; see the
/// module doc comment.
pub async fn dispatch(
    pool: &SqlitePool,
    action: &str,
    raw_args: &Value,
) -> AppResult<ActionOutcome> {
    match action {
        "opportunity.create" => {
            let new: NewOpportunity = parse_args(action, raw_args)?;
            let view = opportunities::create_via_pool(pool, new).await?;
            // Not undoable: there is no delete capability for an opportunity.
            Ok(ActionOutcome::new(to_json(&view)?))
        }
        "opportunity.update" | "opportunity.update_stage" => {
            let (id, rest) = split_id(action, raw_args, "opportunity_id")?;
            let patch: OpportunityPatch = parse_args(action, &rest)?;
            let (view, changes) = opportunities::apply_patch_via_pool(pool, id, patch).await?;
            let outcome = to_json(&view)?;
            Ok(if changes.is_empty() {
                ActionOutcome::new(outcome)
            } else {
                ActionOutcome::with_undo(
                    outcome,
                    Undo::OpportunityFields {
                        change_ids: changes.iter().map(|c| c.id).collect(),
                    },
                )
            })
        }
        "task.create" => {
            let (opportunity_id, rest) = split_id(action, raw_args, "opportunity_id")?;
            let new: NewTask = parse_args(action, &rest)?;
            let task =
                tasks::create_via_pool(pool, opportunity_id, &new.title, new.due_at.as_deref())
                    .await?;
            Ok(ActionOutcome::with_undo(
                to_json(&task)?,
                Undo::DeleteTask { task_id: task.id },
            ))
        }
        "task.complete" => {
            let (task_id, rest) = split_id(action, raw_args, "task_id")?;
            let patch: TaskPatch = parse_args(action, &rest)?;
            let (old, new) = tasks::apply_patch_via_pool(pool, task_id, patch).await?;
            Ok(ActionOutcome::with_undo(
                to_json(&new)?,
                Undo::SetTaskDone {
                    task_id,
                    done: old.done,
                },
            ))
        }
        "lead.accept" => {
            let (lead_id, _rest) = split_id(action, raw_args, "lead_id")?;
            let response = leads::accept_via_pool(pool, lead_id).await?;
            // Not undoable: see `leads::accept_via_pool`'s doc comment.
            Ok(ActionOutcome::new(to_json(&response)?))
        }
        "lead.reject" => {
            let (lead_id, rest) = split_id(action, raw_args, "lead_id")?;
            let a: LeadRejectArgs = parse_args(action, &rest)?;
            let lead = leads::reject_via_pool(pool, lead_id, &a.reason).await?;
            // Not undoable: see `leads::reject_via_pool`'s doc comment.
            Ok(ActionOutcome::new(to_json(&lead)?))
        }
        "lead.snooze" => {
            let (lead_id, rest) = split_id(action, raw_args, "lead_id")?;
            let a: LeadSnoozeArgs = parse_args(action, &rest)?;
            let (prior, new) = leads::snooze_via_pool(pool, lead_id, &a.until).await?;
            Ok(ActionOutcome::with_undo(
                to_json(&new)?,
                Undo::RestoreLead {
                    lead_id,
                    status: prior.status,
                    reject_reason: prior.reject_reason,
                    snoozed_until: prior.snoozed_until,
                },
            ))
        }
        "proposal.accept" => {
            let (id, _rest) = split_id(action, raw_args, "proposal_id")?;
            let response = proposals::accept_via_pool(pool, id).await?;
            Ok(ActionOutcome::with_undo(
                to_json(&response)?,
                Undo::UnacceptProposal { proposal_id: id },
            ))
        }
        "proposal.reject" => {
            let (id, _rest) = split_id(action, raw_args, "proposal_id")?;
            let change = proposals::reject_via_pool(pool, id).await?;
            Ok(ActionOutcome::with_undo(
                to_json(&change)?,
                Undo::UnrejectProposal { proposal_id: id },
            ))
        }
        other => Err(AppError::BadRequest(format!(
            "action `{other}` is not available via chat"
        ))),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LeadRejectArgs {
    reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LeadSnoozeArgs {
    until: String,
}

/// Reverse an applied action. Each variant calls the same reversal logic an
/// equivalent person-driven undo would (see the module doc comment); all of
/// them are safe to retry/replay only once -- a second call sees the
/// already-reverted state and returns a conflict, the same guard the
/// existing `unaccept`/`unreject` endpoints already rely on.
pub async fn undo(pool: &SqlitePool, undo: &Undo) -> AppResult<Value> {
    match undo {
        Undo::OpportunityFields { change_ids } => {
            let mut reverted = Vec::with_capacity(change_ids.len());
            for id in change_ids {
                reverted.push(proposals::unaccept_via_pool(pool, *id).await?);
            }
            to_json(&reverted)
        }
        Undo::DeleteTask { task_id } => {
            tasks::delete_via_pool(pool, *task_id).await?;
            Ok(Value::Null)
        }
        Undo::SetTaskDone { task_id, done } => {
            let patch = TaskPatch {
                title: None,
                due_at: None,
                done: Some(*done),
            };
            let (_old, new) = tasks::apply_patch_via_pool(pool, *task_id, patch).await?;
            to_json(&new)
        }
        Undo::RestoreLead {
            lead_id,
            status,
            reject_reason,
            snoozed_until,
        } => {
            let restored = leads::restore_via_pool(
                pool,
                *lead_id,
                *status,
                reject_reason.as_deref(),
                snoozed_until.as_deref(),
            )
            .await?;
            to_json(&restored)
        }
        Undo::UnacceptProposal { proposal_id } => {
            to_json(&proposals::unaccept_via_pool(pool, *proposal_id).await?)
        }
        Undo::UnrejectProposal { proposal_id } => {
            to_json(&proposals::unreject_via_pool(pool, *proposal_id).await?)
        }
    }
}
