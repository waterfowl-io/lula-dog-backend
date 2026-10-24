//! Configuration from environment variables (a `.env` file is loaded first if
//! present). See `.env.example` for every variable.

use std::fmt;

use anyhow::{Context, bail};
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "sqlite://lula.db";
const DEFAULT_BIND_ADDR: &str = "127.0.0.1:3000";
const DEFAULT_XAI_BASE_URL: &str = "https://api.x.ai/v1";
const DEFAULT_BUCKET_DIR: &str = "data/bucket";
const DEFAULT_LEAD_MIN_FIT_SCORE: i64 = 50;
const DEFAULT_CRAWL_MAX_PAGES: u32 = 4;
const DEFAULT_CRAWL_TIMEOUT_SECS: u64 = 10;
const DEFAULT_CRAWL_MAX_BYTES: u64 = 1_000_000;
const DEFAULT_DATA_DIR: &str = "data";
const DEFAULT_SUPABASE_JWT_AUD: &str = "authenticated";
const DEFAULT_JWKS_TTL_SECS: u64 = 300;
const DEFAULT_QUERY_MAX_ROWS: i64 = 1000;
const DEFAULT_QUERY_TIMEOUT_MS: u64 = 5000;
/// Every workspace other than this one lives under
/// `DATA_DIR/workspaces/{id}.db`; this one is `DATABASE_URL` directly, so a
/// pre-step-3 single-tenant setup keeps reading the same file. See
/// `workspace_db::WorkspaceRegistry`.
pub const DEFAULT_WORKSPACE_ID: Uuid = Uuid::nil();
/// `AUTH_MODE=dev`'s default user id, shared with `cargo run -- seed`'s
/// control-plane bootstrap so the dev user it creates (and the workspace it
/// owns) is exactly the one a locally-running server treats every request
/// as, with no `X-Dev-User` header needed.
pub const DEFAULT_DEV_USER_ID: Uuid = Uuid::from_u128(1);

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
    /// Supabase Postgres control plane: profiles, workspaces, membership,
    /// app definitions, saved queries, schema catalog. `None` disables
    /// workspace-membership enforcement (every workspace id is accessible
    /// as `owner`) -- see `workspace_db::scope`.
    pub control_database_url: Option<String>,
    /// Per-workspace SQLite files live under `{data_dir}/workspaces/{id}.db`.
    pub data_dir: String,
    pub auth: AuthMode,
    /// Hard cap on rows returned by `POST /w/{ws}/queries/{id}/run`.
    pub query_max_rows: i64,
    pub query_timeout_ms: u64,
}

#[derive(Clone)]
pub enum AuthMode {
    /// `AUTH_MODE=supabase` (the default): verify a Supabase Auth JWT.
    Supabase {
        supabase_url: String,
        aud: String,
        /// Legacy HS256 fallback; `None` rejects HS256 tokens outright.
        jwt_secret: Option<String>,
        jwks_ttl_secs: u64,
    },
    /// `AUTH_MODE=dev`: bypass verification. Only reachable by setting the
    /// env var explicitly, never a fallback.
    Dev { default_user_id: Uuid },
}

impl fmt::Debug for AuthMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Supabase {
                supabase_url, aud, ..
            } => f
                .debug_struct("Supabase")
                .field("supabase_url", supabase_url)
                .field("aud", aud)
                .field("jwt_secret", &"<redacted>")
                .finish(),
            Self::Dev { default_user_id } => f
                .debug_struct("Dev")
                .field("default_user_id", default_user_id)
                .finish(),
        }
    }
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
            .field(
                "control_database_url",
                &self.control_database_url.as_ref().map(|_| "<set>"),
            )
            .field("data_dir", &self.data_dir)
            .field("auth", &self.auth)
            .field("query_max_rows", &self.query_max_rows)
            .field("query_timeout_ms", &self.query_timeout_ms)
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
        let control_database_url = var("CONTROL_DATABASE_URL");
        let data_dir = var("DATA_DIR").unwrap_or_else(|| DEFAULT_DATA_DIR.to_owned());
        let auth = match var("AUTH_MODE").as_deref() {
            None | Some("supabase") => AuthMode::Supabase {
                supabase_url: var("SUPABASE_URL")
                    .context("AUTH_MODE=supabase (the default) requires SUPABASE_URL")?,
                aud: var("SUPABASE_JWT_AUD").unwrap_or_else(|| DEFAULT_SUPABASE_JWT_AUD.to_owned()),
                jwt_secret: var("SUPABASE_JWT_SECRET"),
                jwks_ttl_secs: var_parsed("JWKS_CACHE_TTL_SECS", DEFAULT_JWKS_TTL_SECS)?,
            },
            Some("dev") => AuthMode::Dev {
                default_user_id: var("DEV_USER_ID").map_or_else(
                    || Ok(DEFAULT_DEV_USER_ID),
                    |v| v.parse().context("DEV_USER_ID is not a uuid"),
                )?,
            },
            Some(other) => bail!("AUTH_MODE must be `supabase` or `dev`, got `{other}`"),
        };
        let query_max_rows = var_parsed("QUERY_MAX_ROWS", DEFAULT_QUERY_MAX_ROWS)?;
        let query_timeout_ms = var_parsed("QUERY_TIMEOUT_MS", DEFAULT_QUERY_TIMEOUT_MS)?;
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
            control_database_url,
            data_dir,
            auth,
            query_max_rows,
            query_timeout_ms,
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
