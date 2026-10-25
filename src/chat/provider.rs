//! The LLM seam: one model turn in, text/tool-calls out. `Fake` is
//! deterministic (no network) and exercises the real tool-calling loop for
//! tests and local dev; `Grok` calls xAI's OpenAI-compatible chat
//! completions API with streaming, reusing the same request shape
//! `crate::extractor::grok`/`crate::classifier::grok` use for their
//! non-streaming calls.

mod fake;
mod grok;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;

pub use fake::FakeProvider;
pub use grok::GrokProvider;

use super::sse::ChatEvent;
use super::tools::ToolSpec;
use crate::config::ChatProviderConfig;

/// One message in the conversation sent to the provider, in order.
#[derive(Debug, Clone)]
pub enum ProviderMessage {
    System(String),
    User(String),
    Assistant {
        content: Option<String>,
        tool_calls: Vec<ProviderToolCall>,
    },
    Tool {
        tool_call_id: String,
        name: String,
        content: Value,
    },
}

#[derive(Debug, Clone)]
pub struct ProviderToolCall {
    pub id: String,
    pub name: String,
    pub args: Value,
}

/// What one call to `ChatProvider::complete` produced: either a final
/// answer (`content`, no tool calls) or a request to run tools
/// (`tool_calls`, possibly alongside `content` the model said before
/// calling them).
pub struct ProviderTurn {
    pub content: Option<String>,
    pub tool_calls: Vec<ProviderToolCall>,
    /// The resolved model version string, for `chat_messages.model`.
    pub model: String,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("{0}")]
    Request(String),
    #[error("provider returned status {status}: {body}")]
    Status { status: u16, body: String },
    #[error("malformed provider response: {0}")]
    Malformed(String),
}

/// One model call. Implementations stream text as it arrives by sending
/// `ChatEvent::TextDelta` on `deltas` (best-effort: a closed receiver just
/// means the client disconnected, so a send error is not itself fatal to
/// producing the final `ProviderTurn`).
#[async_trait]
pub trait ChatProvider: Send + Sync {
    async fn complete(
        &self,
        messages: &[ProviderMessage],
        tools: &[ToolSpec],
        max_tokens: u32,
        deltas: &mpsc::Sender<ChatEvent>,
    ) -> Result<ProviderTurn, ProviderError>;
}

pub fn provider_from_config(config: &ChatProviderConfig) -> anyhow::Result<Arc<dyn ChatProvider>> {
    Ok(match config {
        ChatProviderConfig::Fake => Arc::new(FakeProvider),
        ChatProviderConfig::Grok {
            api_key,
            model,
            base_url,
        } => Arc::new(GrokProvider::new(api_key.clone(), model.clone(), base_url)?),
    })
}
