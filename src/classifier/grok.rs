//! xAI Grok via the OpenAI-compatible chat completions API, with a strict
//! JSON schema response format, the same pattern as
//! `crate::extractor::GrokExtractor`.

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Classification, ClassificationInput, Classifier, ClassifyError};

// Only the part of the completion response read here. Not
// `deny_unknown_fields`: the provider adds response fields freely.
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

const TIMEOUT: Duration = Duration::from_secs(60);
const ERROR_BODY_MAX_CHARS: usize = 500;

const SYSTEM_PROMPT: &str = "\
You score how well a company fits a packaging-materials salesperson's target \
profile (corrugated boxes, mailers, pallets and similar physical packaging). \
You receive the company record (from a state filing or business directory), \
the salesperson's target profile, and text crawled from the company's \
website (it may be empty if nothing was crawled). Base every judgment on \
what the record and text actually say; never invent facts.
- industry: a short label for what the company does, or null if unclear.
- products: concrete products or services the company sells, if stated.
- ships_physical_goods: true only if the text or record indicates the \
company ships, sells or manufactures physical goods.
- size_signals: short phrases hinting at company size (employee counts, \
revenue, number of locations), if stated.
- contact_page_url: the best URL for a sales contact, using the given hint \
if it looks right, else null.
- fit_score: 0-100, how well this company matches the target profile's \
industries and keywords as a buyer of packaging materials.
- fit_reason: one sentence on why.";

pub struct GrokClassifier {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl GrokClassifier {
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

    fn request_body(&self, input: &ClassificationInput<'_>) -> Value {
        let user = json!({
            "company": input.company,
            "target_profile": input.profile,
            "crawled_text": input.crawled_text,
            "contact_page_hint": input.contact_hint,
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
                    "name": "company_classification",
                    "strict": true,
                    "schema": schema(),
                },
            },
        })
    }
}

fn schema() -> Value {
    let nullable_string = json!({"type": ["string", "null"]});
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "industry": nullable_string,
            "products": {"type": "array", "items": {"type": "string"}},
            "ships_physical_goods": {"type": "boolean"},
            "size_signals": {"type": "array", "items": {"type": "string"}},
            "contact_page_url": nullable_string,
            "fit_score": {"type": "integer"},
            "fit_reason": nullable_string,
        },
        "required": [
            "industry", "products", "ships_physical_goods", "size_signals",
            "contact_page_url", "fit_score", "fit_reason",
        ],
    })
}

#[async_trait]
impl Classifier for GrokClassifier {
    async fn classify(
        &self,
        input: ClassificationInput<'_>,
    ) -> Result<Classification, ClassifyError> {
        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&self.request_body(&input))
            .send()
            .await
            .map_err(|e| ClassifyError::Request(e.without_url().to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ClassifyError::Status {
                status: status.as_u16(),
                body: body.chars().take(ERROR_BODY_MAX_CHARS).collect(),
            });
        }

        let chat: ChatResponse = response
            .json()
            .await
            .map_err(|e| ClassifyError::Malformed(e.without_url().to_string()))?;
        let content = chat
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .ok_or_else(|| ClassifyError::Malformed("no message content".to_owned()))?;
        let parsed: Classification =
            serde_json::from_str(&content).map_err(|e| ClassifyError::Malformed(e.to_string()))?;
        Ok(parsed.sanitize())
    }
}
