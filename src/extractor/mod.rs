//! The AI extractor seam. Given a pasted thread and the opportunity as it
//! stands, an extractor returns what it found: field values, a summary,
//! suggested tasks and the latest date in the thread.
//!
//! Two impls: `StubExtractor` (deterministic heuristics, the default and what
//! tests use) and `GrokExtractor` (xAI). Output is untrusted: the paste flow
//! validates every value and drops what does not normalize.

mod grok;
mod stub;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::opportunities::Opportunity;

pub use grok::GrokExtractor;
pub use stub::StubExtractor;

pub struct ExtractionInput<'a> {
    pub opportunity: &'a Opportunity,
    pub text: &'a str,
    /// Today's date (UTC, `YYYY-MM-DD`), for resolving relative dates.
    pub today: &'a str,
}

/// What an extractor found. Every field is optional; `None` means "not
/// mentioned", never "clear it".
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Extraction {
    pub summary: Option<String>,
    pub company: Option<String>,
    pub contact_name: Option<String>,
    pub contact_email: Option<String>,
    pub value_cents: Option<i64>,
    pub stage: Option<String>,
    pub next_step: Option<String>,
    /// The most recent date in the thread, any format `time::normalize`
    /// accepts.
    pub latest_date: Option<String>,
    #[serde(default)]
    pub tasks: Vec<SuggestedTask>,
    /// The source quote behind each field above, when the extractor can
    /// point to one. Keyed the same way the fields are named here, not by
    /// `proposals::Field`'s column names, since `contact_name` and
    /// `contact_email` usually share one quote (the `From:` line).
    #[serde(default)]
    pub evidence: Evidence,
}

/// Per-field evidence quotes, parallel to `Extraction`'s proposable fields
/// (everything except `summary`, `latest_date` and `tasks`, which aren't
/// proposals).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    #[serde(default)]
    pub company: Option<String>,
    #[serde(default)]
    pub contact_name: Option<String>,
    #[serde(default)]
    pub contact_email: Option<String>,
    #[serde(default)]
    pub value_cents: Option<String>,
    #[serde(default)]
    pub stage: Option<String>,
    #[serde(default)]
    pub next_step: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuggestedTask {
    pub title: String,
    pub due_at: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("extractor request failed: {0}")]
    Request(String),
    #[error("extractor returned HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error("extractor returned malformed output: {0}")]
    Malformed(String),
}

#[async_trait]
pub trait Extractor: Send + Sync {
    async fn extract(&self, input: ExtractionInput<'_>) -> Result<Extraction, ExtractError>;
}
