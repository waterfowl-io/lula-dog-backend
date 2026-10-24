//! Configuration from environment variables (a `.env` file is loaded first if
//! present). See `.env.example` for every variable.

use std::fmt;

use anyhow::{Context, bail};

const DEFAULT_DATABASE_URL: &str = "sqlite://lula.db";
const DEFAULT_BIND_ADDR: &str = "127.0.0.1:3000";
const DEFAULT_XAI_BASE_URL: &str = "https://api.x.ai/v1";

#[derive(Clone)]
pub struct Config {
    pub database_url: String,
    pub bind_addr: String,
    pub extractor: ExtractorConfig,
}

#[derive(Clone)]
pub enum ExtractorConfig {
    /// Deterministic heuristics, no network. The default.
    Stub,
    /// xAI Grok via the OpenAI-compatible chat completions API.
    Grok {
        api_key: String,
        model: String,
        base_url: String,
    },
}

// Hand-written so the API key can never reach a log line.
impl fmt::Debug for ExtractorConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stub => f.write_str("Stub"),
            Self::Grok {
                model, base_url, ..
            } => f
                .debug_struct("Grok")
                .field("api_key", &"<redacted>")
                .field("model", model)
                .field("base_url", base_url)
                .finish(),
        }
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("database_url", &self.database_url)
            .field("bind_addr", &self.bind_addr)
            .field("extractor", &self.extractor)
            .finish()
    }
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let database_url = var("DATABASE_URL").unwrap_or_else(|| DEFAULT_DATABASE_URL.to_owned());
        let bind_addr = var("BIND_ADDR").unwrap_or_else(|| DEFAULT_BIND_ADDR.to_owned());
        let extractor = match var("EXTRACTOR").as_deref() {
            None | Some("stub") => ExtractorConfig::Stub,
            Some("grok") => ExtractorConfig::Grok {
                api_key: var("XAI_API_KEY").context("EXTRACTOR=grok requires XAI_API_KEY")?,
                model: var("XAI_MODEL").context("EXTRACTOR=grok requires XAI_MODEL")?,
                base_url: var("XAI_BASE_URL").unwrap_or_else(|| DEFAULT_XAI_BASE_URL.to_owned()),
            },
            Some(other) => bail!("EXTRACTOR must be `stub` or `grok`, got `{other}`"),
        };
        Ok(Self {
            database_url,
            bind_addr,
            extractor,
        })
    }
}

/// A set, non-empty environment variable.
fn var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}
