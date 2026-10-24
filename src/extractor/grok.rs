//! xAI Grok via the OpenAI-compatible chat completions API, with a strict
//! JSON schema as the response format so the reply parses straight into
//! `Extraction`.

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ExtractError, Extraction, ExtractionInput, Extractor};

const TIMEOUT: Duration = Duration::from_secs(60);
const ERROR_BODY_MAX_CHARS: usize = 500;

const SYSTEM_PROMPT: &str = "\
You extract structured sales data for a packaging-materials salesperson \
(corrugated boxes, mailers, pallets). You receive the current opportunity \
record and a pasted email thread or call notes. Report only what the text \
states; use null for anything not mentioned. Never invent contacts, amounts \
or dates.
- summary: one sentence on what this text says about the deal.
- company, contact_name, contact_email: the customer side, not the salesperson.
- value_cents: total deal value in US cents if a price or total is stated.
- stage: one of lead, qualified, proposal, won, lost, only if the text makes it clear.
- next_step: the next concrete action for the salesperson.
- latest_date: the date of the most recent message or event in the text, \
ISO 8601. Resolve relative dates against `today`.
- tasks: concrete follow-ups for the salesperson, with an ISO 8601 due date \
when one is stated or clearly implied.";

pub struct GrokExtractor {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl GrokExtractor {
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

    fn request_body(&self, input: &ExtractionInput<'_>) -> Value {
        let user = json!({
            "today": input.today,
            "current_opportunity": input.opportunity,
            "text": input.text,
        });
        json!({
            "model": self.model,
            "temperature": 0,
            "messages": [
                {"role": "system", "content": SYSTEM_PROMPT},
                {"role": "user", "content": user.to_string()},
            ],
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "opportunity_extraction",
                    "strict": true,
                    "schema": schema(),
                },
            },
        })
    }
}

/// The JSON schema for `Extraction`. Strict mode wants every property listed
/// as required, with optional values expressed as nullable.
fn schema() -> Value {
    let nullable_string = json!({"type": ["string", "null"]});
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "summary": nullable_string,
            "company": nullable_string,
            "contact_name": nullable_string,
            "contact_email": nullable_string,
            "value_cents": {"type": ["integer", "null"]},
            "stage": {
                "type": ["string", "null"],
                "enum": ["lead", "qualified", "proposal", "won", "lost", null],
            },
            "next_step": nullable_string,
            "latest_date": nullable_string,
            "tasks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "title": {"type": "string"},
                        "due_at": nullable_string,
                    },
                    "required": ["title", "due_at"],
                },
            },
        },
        "required": [
            "summary", "company", "contact_name", "contact_email", "value_cents",
            "stage", "next_step", "latest_date", "tasks",
        ],
    })
}

// Only the part of the completion response we read. Not `deny_unknown_fields`:
// the provider adds response fields freely. The model's content, which our
// schema governs, is parsed into `Extraction`, which does deny them.
#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: Message,
}

#[derive(Deserialize)]
struct Message {
    content: Option<String>,
}

#[async_trait]
impl Extractor for GrokExtractor {
    async fn extract(&self, input: ExtractionInput<'_>) -> Result<Extraction, ExtractError> {
        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&self.request_body(&input))
            .send()
            .await
            .map_err(|e| ExtractError::Request(e.without_url().to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ExtractError::Status {
                status: status.as_u16(),
                body: body.chars().take(ERROR_BODY_MAX_CHARS).collect(),
            });
        }

        let chat: ChatResponse = response
            .json()
            .await
            .map_err(|e| ExtractError::Malformed(e.without_url().to_string()))?;
        let content = chat
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .ok_or_else(|| ExtractError::Malformed("no message content".to_owned()))?;
        serde_json::from_str(&content).map_err(|e| ExtractError::Malformed(e.to_string()))
    }
}
