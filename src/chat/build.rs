//! Build-mode chat: the assistant edits the app's own definition (pages,
//! layout, components -- see `crate::app_definition`), never its data. Every
//! change lands as a new, schema-validated DRAFT `app_definition_versions`
//! row (`crate::control::app_definitions::create_version`); there is no tool
//! that publishes or discards one -- only the user can, via the ordinary
//! `/versions/{version}/publish` and `/versions/{version}/discard`
//! endpoints `crate::apps` already exposes.
//!
//! `BuildAgent` is the seam this is built behind: today `LlmBuildAgent`
//! drives the same `ChatProvider` as data mode through a small tool-calling
//! loop (`get_app_definition` / `propose_app_definition` / `edit_pipeline`).
//! The long-term plan (`/workspace/lula-agents/agents-design.md`, "Grok
//! Build") is a real coding agent talking ACP that edits the app's React
//! source directly instead of this definition-as-data loop -- a wholly
//! different execution model (its own sandbox, its own notion of a "tool
//! call"), which is why the trait is drawn at "run one build turn", not at
//! "complete one LLM call": swapping in an ACP-backed agent later means
//! writing a new `BuildAgent` impl and pointing `AppState::build_agent` at
//! it, never touching `crate::chat::handlers::stream_chat`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::app_definition::{self, AppDefinitionDoc, Component, Page};
use crate::config::ChatConfig;
use crate::control::ControlPool;
use crate::control::app_definitions::{self, AppDefinition, AppDefinitionVersion};
use crate::error::{AppError, AppResult};

use super::handlers;
use super::provider::{ChatProvider, ProviderMessage, ProviderToolCall};
use super::sse::{ChatEvent, Usage};
use super::storage::{self, ToolCallRecord};
use super::tools::{self, ToolSpec};

/// Everything one build-mode turn needs, already scoped to the caller's
/// workspace, app and role (`crate::chat::handlers::stream_chat` 403s a
/// non-admin/owner before this is ever built).
pub struct BuildTurnContext {
    /// The workspace's own SQLite pool -- chat conversations/messages are
    /// customer data, stored here exactly like data mode (see
    /// `crate::chat::storage`), even though the app definition itself lives
    /// in the control plane.
    pub pool: SqlitePool,
    pub control: ControlPool,
    pub workspace_id: Uuid,
    pub app: AppDefinition,
    /// The definition this turn starts editing from: the published version,
    /// or the latest non-discarded draft if nothing is published yet (see
    /// `app_definitions::fetch_published_or_latest`).
    pub base_version: AppDefinitionVersion,
    pub conversation_id: String,
    pub user_id: Uuid,
}

#[async_trait]
pub trait BuildAgent: Send + Sync {
    /// Run one build-mode turn: react to the user's latest message (already
    /// stored by the caller, and the last entry of `history`), streaming
    /// `ChatEvent`s as it goes. May create any number of draft versions
    /// (each one a `ChatEvent::DraftCreated`); must never publish one.
    async fn run_turn(
        &self,
        ctx: BuildTurnContext,
        history: Vec<ProviderMessage>,
        tx: tokio::sync::mpsc::Sender<ChatEvent>,
    );
}

pub fn agent_from_config(
    provider: Arc<dyn ChatProvider>,
    config: &ChatConfig,
) -> Arc<dyn BuildAgent> {
    Arc::new(LlmBuildAgent {
        provider,
        config: config.clone(),
    })
}

/// The build-mode tool set. Unlike data mode's one-write-tool-per-action
/// shape, there is exactly one write tool (`propose_app_definition`): a
/// definition is one document, not a set of independent fields, so "give me
/// the whole new document" is both simpler for the model and the only way
/// `crate::app_definition::validate` can check it as a whole (duplicate ids,
/// dangling layout refs, etc. are all cross-field checks).
pub fn build_mode_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "get_app_definition",
            description: "Get the app's current definition (pages, layout, components) as JSON \
                -- including any draft already proposed earlier this turn.",
            parameters: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: "propose_app_definition",
            description: "Save a new draft version of the app definition. `definition` must be \
                the FULL updated document, not a patch -- it replaces the current one entirely. \
                Validated before it's stored (schema shape, unique ids, every action/saved-query \
                reference resolves); an invalid definition comes back as an error for you to fix \
                and retry, never stored as a draft. Never publishes -- only the user can, from \
                the app, after reviewing the diff summary this returns.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "definition": {"type": "object", "description": "the full app definition document"},
                },
                "required": ["definition"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: "edit_pipeline",
            description: "Edit this app's ingress/egress data pipeline (how data flows in from \
                sources and out to destinations). Not built yet -- always returns a \"coming \
                soon\" result; tell the user instead of retrying.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "request": {"type": "string", "description": "what the user asked to change"},
                },
                "additionalProperties": false,
            }),
        },
    ]
}

/// `edit_pipeline`'s fixed result. Pipeline execution doesn't exist yet
/// (see `/workspace/lula-pipelines/pipelines-design.md`); when it lands,
/// this is where a real implementation plugs in -- parse `request`, resolve
/// the app's pipeline config (new control-plane tables, not
/// `app_definition_versions`: a pipeline isn't rendered UI), validate and
/// save a draft pipeline change the same way `propose_app_definition` does
/// for the UI, and emit its own `ChatEvent` variant alongside
/// `DraftCreated`.
fn edit_pipeline_stub() -> Value {
    json!({
        "status": "coming_soon",
        "message": "Ingress/egress pipeline editing isn't built yet -- only the app's pages and \
            components can be edited from chat today. See \
            /workspace/lula-pipelines/pipelines-design.md for the pipeline design this will plug \
            into.",
    })
}

/// One build tool call's outcome: either just a result for the model, or a
/// result that also minted a new draft version (so the caller can emit
/// `DraftCreated` and record it).
enum BuildToolOutcome {
    Result(Value),
    DraftCreated {
        version: AppDefinitionVersion,
        diff_summary: String,
        result: Value,
    },
}

#[derive(Deserialize)]
struct ProposeArgs {
    definition: Value,
}

async fn propose_app_definition(
    ctx: &BuildTurnContext,
    current_definition: &Value,
    raw_args: &Value,
) -> AppResult<BuildToolOutcome> {
    let args: ProposeArgs = match tools::parse_tool_args(raw_args) {
        Ok(args) => args,
        Err(e) => return Ok(BuildToolOutcome::Result(tools::soft_error(e)?)),
    };
    let doc: AppDefinitionDoc = match serde_json::from_value(args.definition.clone()) {
        Ok(doc) => doc,
        Err(e) => {
            return Ok(BuildToolOutcome::Result(json!({
                "error": format!("invalid app definition shape: {e}")
            })));
        }
    };
    if let Err(e) = app_definition::validate(&ctx.control, ctx.workspace_id, &doc).await {
        return Ok(BuildToolOutcome::Result(tools::soft_error(e)?));
    }
    let old_doc: AppDefinitionDoc = serde_json::from_value(current_definition.clone())
        .map_err(|e| AppError::Internal(format!("re-parsing the working definition: {e}")))?;
    let summary = diff_summary(&old_doc, &doc);
    let version = app_definitions::create_version(
        &ctx.control,
        ctx.app.id,
        args.definition.clone(),
        Some(ctx.user_id),
    )
    .await?;
    let result = json!({
        "version_id": version.id,
        "version": version.version,
        "diff_summary": summary,
    });
    Ok(BuildToolOutcome::DraftCreated {
        version,
        diff_summary: summary,
        result,
    })
}

async fn execute(
    ctx: &BuildTurnContext,
    current_definition: &Value,
    name: &str,
    raw_args: Value,
) -> AppResult<BuildToolOutcome> {
    match name {
        "get_app_definition" => Ok(BuildToolOutcome::Result(
            json!({ "definition": current_definition }),
        )),
        "propose_app_definition" => {
            propose_app_definition(ctx, current_definition, &raw_args).await
        }
        "edit_pipeline" => Ok(BuildToolOutcome::Result(edit_pipeline_stub())),
        other => Ok(BuildToolOutcome::Result(json!({
            "error": format!("unknown tool `{other}`")
        }))),
    }
}

/// A human-readable, page-by-page summary of what changed between two app
/// definitions, for the draft card's diff and the model's own confirmation
/// text. Pure data comparison (added/removed pages, components, nav items; a
/// changed component is reported by id, not field-by-field) -- good enough
/// for a chat summary without needing a real structural diff library.
fn diff_summary(old: &AppDefinitionDoc, new: &AppDefinitionDoc) -> String {
    let old_pages: BTreeMap<&str, &Page> = old.pages.iter().map(|p| (p.id.as_str(), p)).collect();
    let new_pages: BTreeMap<&str, &Page> = new.pages.iter().map(|p| (p.id.as_str(), p)).collect();
    let mut lines = Vec::new();

    for (id, page) in &new_pages {
        if !old_pages.contains_key(id) {
            lines.push(format!("added page `{id}` ({})", page.title));
        }
    }
    for (id, page) in &old_pages {
        if !new_pages.contains_key(id) {
            lines.push(format!("removed page `{id}` ({})", page.title));
        }
    }
    for (id, new_page) in &new_pages {
        let Some(old_page) = old_pages.get(id) else {
            continue;
        };
        let old_components: BTreeMap<&str, &Component> = old_page
            .components
            .iter()
            .map(|c| (c.id.as_str(), c))
            .collect();
        let new_components: BTreeMap<&str, &Component> = new_page
            .components
            .iter()
            .map(|c| (c.id.as_str(), c))
            .collect();
        for (cid, c) in &new_components {
            if !old_components.contains_key(cid) {
                lines.push(format!("page `{id}`: added {:?} component `{cid}`", c.kind));
            }
        }
        for cid in old_components.keys() {
            if !new_components.contains_key(cid) {
                lines.push(format!("page `{id}`: removed component `{cid}`"));
            }
        }
        for (cid, new_c) in &new_components {
            if let Some(old_c) = old_components.get(cid)
                && serde_json::to_value(old_c).ok() != serde_json::to_value(new_c).ok()
            {
                lines.push(format!("page `{id}`: changed component `{cid}`"));
            }
        }
    }

    let old_nav: BTreeMap<&str, &str> = old
        .nav
        .iter()
        .map(|n| (n.path.as_str(), n.label.as_str()))
        .collect();
    let new_nav: BTreeMap<&str, &str> = new
        .nav
        .iter()
        .map(|n| (n.path.as_str(), n.label.as_str()))
        .collect();
    for (path, label) in &new_nav {
        if !old_nav.contains_key(path) {
            lines.push(format!("added nav item \"{label}\" -> `{path}`"));
        }
    }
    for path in old_nav.keys() {
        if !new_nav.contains_key(path) {
            lines.push(format!("removed nav item `{path}`"));
        }
    }

    if lines.is_empty() {
        "No visible changes.".to_owned()
    } else {
        lines.join("\n")
    }
}

/// Drives `ChatProvider` through `build_mode_tools()` the same shape data
/// mode's `Turn` drives `data_mode_tools()` -- see the module doc comment
/// for why this whole turn, not just the provider call, is behind
/// `BuildAgent`.
struct LlmBuildAgent {
    provider: Arc<dyn ChatProvider>,
    config: ChatConfig,
}

#[async_trait]
impl BuildAgent for LlmBuildAgent {
    async fn run_turn(
        &self,
        ctx: BuildTurnContext,
        mut history: Vec<ProviderMessage>,
        tx: tokio::sync::mpsc::Sender<ChatEvent>,
    ) {
        let tool_specs = build_mode_tools();
        let mut current_definition = ctx.base_version.definition.clone();

        for _round in 0..self.config.max_tool_rounds {
            let started = Instant::now();
            let turn = match self
                .provider
                .complete(&history, &tool_specs, self.config.max_tokens, &tx)
                .await
            {
                Ok(turn) => turn,
                Err(e) => {
                    handlers::send_error(&tx, format!("provider error: {e}")).await;
                    return;
                }
            };
            let latency_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
            let provider_name = handlers::provider_label(&self.config.provider);
            let cost_usd =
                handlers::compute_cost(&self.config, turn.prompt_tokens, turn.completion_tokens);

            let tool_call_records: Vec<ToolCallRecord> = turn
                .tool_calls
                .iter()
                .map(|call| ToolCallRecord {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    args: call.args.clone(),
                })
                .collect();

            let assistant_message = match storage::insert_assistant_message(
                &ctx.pool,
                &ctx.conversation_id,
                storage::AssistantTurn {
                    content: turn.content.as_deref(),
                    tool_calls: &tool_call_records,
                    provider: provider_name,
                    model: &turn.model,
                    prompt_tokens: turn.prompt_tokens,
                    completion_tokens: turn.completion_tokens,
                    cost_usd,
                    latency_ms,
                },
            )
            .await
            {
                Ok(message) => message,
                Err(e) => {
                    handlers::send_error(&tx, format!("storage error: {e}")).await;
                    return;
                }
            };

            history.push(ProviderMessage::Assistant {
                content: turn.content.clone(),
                tool_calls: turn.tool_calls.clone(),
            });

            if turn.tool_calls.is_empty() {
                let _ignored = tx
                    .send(ChatEvent::Final {
                        message_id: assistant_message.id,
                        usage: Usage {
                            provider: provider_name.to_owned(),
                            model: turn.model,
                            prompt_tokens: turn.prompt_tokens,
                            completion_tokens: turn.completion_tokens,
                            cost_usd,
                            latency_ms,
                        },
                    })
                    .await;
                return;
            }

            for call in turn.tool_calls {
                if !self
                    .run_tool_call(&ctx, &call, &mut current_definition, &tx, &mut history)
                    .await
                {
                    return;
                }
            }
        }
        handlers::send_error(
            &tx,
            "reached the tool-call round limit without a final answer".to_owned(),
        )
        .await;
    }
}

impl LlmBuildAgent {
    /// Run one tool call, store and emit its result, push it onto
    /// `history`. Returns `false` on a hard (infra) error, which already
    /// sent an `error` event and means the caller should stop the turn.
    async fn run_tool_call(
        &self,
        ctx: &BuildTurnContext,
        call: &ProviderToolCall,
        current_definition: &mut Value,
        tx: &tokio::sync::mpsc::Sender<ChatEvent>,
        history: &mut Vec<ProviderMessage>,
    ) -> bool {
        let _ignored = tx
            .send(ChatEvent::ToolCallStart {
                id: call.id.clone(),
                name: call.name.clone(),
                args: call.args.clone(),
            })
            .await;

        let outcome = match execute(ctx, current_definition, &call.name, call.args.clone()).await {
            Ok(outcome) => outcome,
            Err(e) => {
                handlers::send_error(tx, format!("tool error: {e}")).await;
                return false;
            }
        };

        let (result, draft) = match outcome {
            BuildToolOutcome::Result(result) => (result, None),
            BuildToolOutcome::DraftCreated {
                version,
                diff_summary,
                result,
            } => {
                *current_definition = version.definition.clone();
                (result, Some((version, diff_summary)))
            }
        };

        if let Err(e) = storage::insert_tool_message(
            &ctx.pool,
            &ctx.conversation_id,
            &call.id,
            &call.name,
            &result,
        )
        .await
        {
            handlers::send_error(tx, format!("storage error: {e}")).await;
            return false;
        }
        let _ignored = tx
            .send(ChatEvent::ToolResult {
                id: call.id.clone(),
                name: call.name.clone(),
                result: result.clone(),
            })
            .await;

        if let Some((version, diff_summary)) = draft {
            let _ignored = tx
                .send(ChatEvent::DraftCreated {
                    version_id: version.id.to_string(),
                    diff_summary,
                })
                .await;
        }

        history.push(ProviderMessage::Tool {
            tool_call_id: call.id.clone(),
            name: call.name.clone(),
            content: result,
        });
        true
    }
}
