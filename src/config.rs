//! Configuration from environment variables (a `.env` file is loaded first if
//! present). See `.env.example` for every variable.

use std::fmt;

use anyhow::{Context, bail};

const DEFAULT_DATABASE_URL: &str = "sqlite://lula.db";
const DEFAULT_BIND_ADDR: &str = "127.0.0.1:3000";
const DEFAULT_XAI_BASE_URL: &str = "https://api.x.ai/v1";
const DEFAULT_BUCKET_DIR: &str = "data/bucket";
const DEFAULT_LEAD_MIN_FIT_SCORE: i64 = 50;
const DEFAULT_CRAWL_MAX_PAGES: u32 = 4;
const DEFAULT_CRAWL_TIMEOUT_SECS: u64 = 10;
const DEFAULT_CRAWL_MAX_BYTES: u64 = 1_000_000;

#[derive(Clone)]
pub struct Config {
    pub database_url: String,
    pub bind_addr: String,
    pub extractor: ExtractorConfig,
    /// Local directory raw source files and crawled pages are archived under.
    pub bucket_dir: String,
    pub places: PlacesConfig,
    pub crawler: CrawlerConfig,
    pub classifier: ClassifierConfig,
    pub crawl_limits: CrawlLimits,
    /// Default `target_profiles.min_fit_score` when seeding Jack's profile.
    pub lead_min_fit_score: i64,
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

#[derive(Clone)]
pub enum PlacesConfig {
    /// Deterministic, no network. The default.
    Stub,
    /// Google Places Text Search (New).
    Google { api_key: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrawlerConfig {
    /// Deterministic, no network. The default.
    Stub,
    /// A real HTTP fetcher, respecting `crawl_limits`.
    Http,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrawlLimits {
    /// Pages fetched per company, including the home page.
    pub max_pages: u32,
    pub timeout_secs: u64,
    /// Response bodies larger than this are truncated before parsing.
    pub max_bytes: u64,
}

#[derive(Clone)]
pub enum ClassifierConfig {
    /// Deterministic heuristics, no network. The default.
    Stub,
    /// xAI Grok via the OpenAI-compatible chat completions API, same
    /// credentials as `ExtractorConfig::Grok`.
    Grok {
        api_key: String,
        model: String,
        base_url: String,
    },
}

// Hand-written so API keys can never reach a log line.
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

impl fmt::Debug for PlacesConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stub => f.write_str("Stub"),
            Self::Google { .. } => f
                .debug_struct("Google")
                .field("api_key", &"<redacted>")
                .finish(),
        }
    }
}

impl fmt::Debug for ClassifierConfig {
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
            .field("bucket_dir", &self.bucket_dir)
            .field("places", &self.places)
            .field("crawler", &self.crawler)
            .field("classifier", &self.classifier)
            .field("crawl_limits", &self.crawl_limits)
            .field("lead_min_fit_score", &self.lead_min_fit_score)
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
        let bucket_dir = var("BUCKET_DIR").unwrap_or_else(|| DEFAULT_BUCKET_DIR.to_owned());
        let places = match var("PLACES").as_deref() {
            None | Some("stub") => PlacesConfig::Stub,
            Some("google") => PlacesConfig::Google {
                api_key: var("GOOGLE_PLACES_API_KEY")
                    .context("PLACES=google requires GOOGLE_PLACES_API_KEY")?,
            },
            Some(other) => bail!("PLACES must be `stub` or `google`, got `{other}`"),
        };
        let crawler = match var("CRAWLER").as_deref() {
            None | Some("stub") => CrawlerConfig::Stub,
            Some("http") => CrawlerConfig::Http,
            Some(other) => bail!("CRAWLER must be `stub` or `http`, got `{other}`"),
        };
        let classifier = match var("CLASSIFIER").as_deref() {
            None | Some("stub") => ClassifierConfig::Stub,
            Some("grok") => ClassifierConfig::Grok {
                api_key: var("XAI_API_KEY").context("CLASSIFIER=grok requires XAI_API_KEY")?,
                model: var("XAI_MODEL").context("CLASSIFIER=grok requires XAI_MODEL")?,
                base_url: var("XAI_BASE_URL").unwrap_or_else(|| DEFAULT_XAI_BASE_URL.to_owned()),
            },
            Some(other) => bail!("CLASSIFIER must be `stub` or `grok`, got `{other}`"),
        };
        let crawl_limits = CrawlLimits {
            max_pages: var_parsed("CRAWL_MAX_PAGES", DEFAULT_CRAWL_MAX_PAGES)?,
            timeout_secs: var_parsed("CRAWL_TIMEOUT_SECS", DEFAULT_CRAWL_TIMEOUT_SECS)?,
            max_bytes: var_parsed("CRAWL_MAX_BYTES", DEFAULT_CRAWL_MAX_BYTES)?,
        };
        let lead_min_fit_score = var_parsed("LEAD_MIN_FIT_SCORE", DEFAULT_LEAD_MIN_FIT_SCORE)?;
        Ok(Self {
            database_url,
            bind_addr,
            extractor,
            bucket_dir,
            places,
            crawler,
            classifier,
            crawl_limits,
            lead_min_fit_score,
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

/// A set environment variable parsed as `T`, or `default` when unset.
fn var_parsed<T: std::str::FromStr>(name: &str, default: T) -> anyhow::Result<T>
where
    T::Err: std::fmt::Display,
{
    var(name).map_or_else(
        || Ok(default),
        |v| {
            v.parse()
                .map_err(|e| anyhow::anyhow!("{name} is invalid: {e}"))
        },
    )
}
