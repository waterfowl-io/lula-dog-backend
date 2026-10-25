//! xAI Grok via the OpenAI-compatible chat completions API, streamed. Same
//! auth/request shape as `crate::extractor::grok`/`crate::classifier::grok`,
//! but with `stream: true` and a `tools` array instead of a strict JSON
//! schema response format.
//!
//! Not exercised by any test in this phase (no `XAI_API_KEY` on this box --
//! see the brief); reviewed carefully against the OpenAI/xAI streaming
//! chunk format instead. Revisit once there's a key to run it against for
//! real.

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::StreamExt;

use super::{ChatProvider, ProviderError, ProviderMessage, ProviderToolCall, ProviderTurn};
use crate::chat::sse::ChatEvent;
use crate::chat::tools::ToolSpec;

const TIMEOUT: Duration = Duration::from_secs(120);
const ERROR_BODY_MAX_CHARS: usize = 500;

pub struct GrokProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl GrokProvider {
    /// `base_url` is the API root, e.g. `https://api.x.ai/v1`.
    pub fn new(api_key: String, model: String, base_url: &str) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder().timeout(TIMEOUT).build()?;
        Ok(Self {
            client,
            api_key,
            model,
            base_url: base_url.trim_end_matches('/').to_owned(),
        })
    }

    fn request_body(
        &self,
        messages: &[ProviderMessage],
        tools: &[ToolSpec],
        max_tokens: u32,
    ) -> Value {
        json!({
            "model": self.model,
            "messages": messages.iter().map(message_to_json).collect::<Vec<_>>(),
            "tools": tools.iter().map(tool_to_json).collect::<Vec<_>>(),
            "tool_choice": "auto",
            "max_tokens": max_tokens,
            "stream": true,
            "stream_options": {"include_usage": true},
        })
    }
}

fn message_to_json(message: &ProviderMessage) -> Value {
    match message {
        ProviderMessage::System(content) => json!({"role": "system", "content": content}),
        ProviderMessage::User(content) => json!({"role": "user", "content": content}),
        ProviderMessage::Assistant {
            content,
            tool_calls,
        } => {
            let calls: Vec<Value> = tool_calls
                .iter()
                .map(|call| {
                    json!({
                        "id": call.id,
                        "type": "function",
                        "function": {"name": call.name, "arguments": call.args.to_string()},
                    })
                })
                .collect();
            if calls.is_empty() {
                json!({"role": "assistant", "content": content})
            } else {
                json!({"role": "assistant", "content": content, "tool_calls": calls})
            }
        }
        ProviderMessage::Tool {
            tool_call_id,
            content,
            ..
        } => json!({
            "role": "tool",
            "tool_call_id": tool_call_id,
            "content": content.to_string(),
        }),
    }
}

fn tool_to_json(tool: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.parameters,
        },
    })
}

#[derive(Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<StreamUsage>,
    #[serde(default)]
    choices: Vec<StreamChoice>,
}

#[derive(Deserialize)]
struct StreamUsage {
    prompt_tokens: i64,
    completion_tokens: i64,
}

#[derive(Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
}

#[derive(Default, Deserialize)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<StreamToolCallDelta>,
}

#[derive(Deserialize)]
struct StreamToolCallDelta {
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<StreamFunctionDelta>,
}

#[derive(Deserialize)]
struct StreamFunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[async_trait]
impl ChatProvider for GrokProvider {
    async fn complete(
        &self,
        messages: &[ProviderMessage],
        tools: &[ToolSpec],
        max_tokens: u32,
        deltas: &mpsc::Sender<ChatEvent>,
    ) -> Result<ProviderTurn, ProviderError> {
        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&self.request_body(messages, tools, max_tokens))
            .send()
            .await
            .map_err(|e| ProviderError::Request(e.without_url().to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Status {
                status: status.as_u16(),
                body: body.chars().take(ERROR_BODY_MAX_CHARS).collect(),
            });
        }

        let mut byte_stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut content = String::new();
        let mut tool_calls: Vec<PartialToolCall> = Vec::new();
        let mut model = self.model.clone();
        let mut prompt_tokens = None;
        let mut completion_tokens = None;

        while let Some(chunk) = byte_stream.next().await {
            let chunk = chunk.map_err(|e| ProviderError::Request(e.without_url().to_string()))?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(end) = buffer.find("\n\n") {
                let event: String = buffer.drain(..=end + 1).collect();
                for line in event.lines() {
                    let Some(data) = line.strip_prefix("data: ") else {
                        continue;
                    };
                    if data == "[DONE]" {
                        continue;
                    }
                    let parsed: StreamChunk = serde_json::from_str(data)
                        .map_err(|e| ProviderError::Malformed(e.to_string()))?;
                    if let Some(m) = parsed.model {
                        model = m;
                    }
                    if let Some(usage) = parsed.usage {
                        prompt_tokens = Some(usage.prompt_tokens);
                        completion_tokens = Some(usage.completion_tokens);
                    }
                    let Some(choice) = parsed.choices.into_iter().next() else {
                        continue;
                    };
                    if let Some(piece) = choice.delta.content {
                        content.push_str(&piece);
                        let _ignored = deltas.send(ChatEvent::TextDelta { text: piece }).await;
                    }
                    for delta in choice.delta.tool_calls {
                        if tool_calls.len() <= delta.index {
                            tool_calls.resize_with(delta.index + 1, PartialToolCall::default);
                        }
                        let Some(slot) = tool_calls.get_mut(delta.index) else {
                            continue;
                        };
                        if let Some(id) = delta.id {
                            slot.id = id;
                        }
                        if let Some(function) = delta.function {
                            if let Some(name) = function.name {
                                slot.name = name;
                            }
                            if let Some(arguments) = function.arguments {
                                slot.arguments.push_str(&arguments);
                            }
                        }
                    }
                }
            }
        }

        let tool_calls = tool_calls
            .into_iter()
            .filter(|call| !call.name.is_empty())
            .map(|call| ProviderToolCall {
                id: call.id,
                name: call.name,
                args: serde_json::from_str(&call.arguments)
                    .unwrap_or_else(|_| Value::Object(serde_json::Map::new())),
            })
            .collect();

        Ok(ProviderTurn {
            content: (!content.is_empty()).then_some(content),
            tool_calls,
            model,
            prompt_tokens,
            completion_tokens,
        })
    }
}
