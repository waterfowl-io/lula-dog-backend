//! The SSE wire format streamed back over `POST .../chat`: `text_delta`,
//! `tool_call`, `tool_result`, `action_applied` (data mode), `draft_created`
//! (build mode -- see `crate::chat::build`), a final `final` event with
//! usage, and `error`.

use axum::response::sse::Event;
use serde::Serialize;
use serde_json::{Value, json};

use super::storage::ChatAction;

#[derive(Debug, Clone, Serialize)]
pub struct Usage {
    pub provider: String,
    pub model: String,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    pub latency_ms: i64,
}

#[derive(Debug, Clone)]
pub enum ChatEvent {
    TextDelta {
        text: String,
    },
    ToolCallStart {
        id: String,
        name: String,
        args: Value,
    },
    ToolResult {
        id: String,
        name: String,
        result: Value,
    },
    /// A write tool call that actually applied, in data mode.
    ActionApplied {
        action: ChatAction,
    },
    /// A new draft app-definition version, in build mode.
    DraftCreated {
        version_id: String,
        diff_summary: String,
    },
    Final {
        message_id: String,
        usage: Usage,
    },
    Error {
        message: String,
    },
}

impl ChatEvent {
    /// The SSE wire encoding: `event: <name>` plus a JSON `data:` payload.
    pub fn into_sse(self) -> Event {
        let (name, data) = match self {
            Self::TextDelta { text } => ("text_delta", json!({ "text": text })),
            Self::ToolCallStart { id, name, args } => {
                ("tool_call", json!({ "id": id, "name": name, "args": args }))
            }
            Self::ToolResult { id, name, result } => (
                "tool_result",
                json!({ "id": id, "name": name, "result": result }),
            ),
            Self::ActionApplied { action } => (
                "action_applied",
                serde_json::to_value(action).unwrap_or(Value::Null),
            ),
            Self::DraftCreated {
                version_id,
                diff_summary,
            } => (
                "draft_created",
                json!({ "version_id": version_id, "diff_summary": diff_summary }),
            ),
            Self::Final { message_id, usage } => {
                ("final", json!({ "message_id": message_id, "usage": usage }))
            }
            Self::Error { message } => ("error", json!({ "message": message })),
        };
        Event::default()
            .event(name)
            .json_data(data)
            .unwrap_or_else(|_| {
                Event::default()
                    .event("error")
                    .data(r#"{"message":"failed to encode event"}"#)
            })
    }
}
