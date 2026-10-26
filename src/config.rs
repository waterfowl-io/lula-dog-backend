//! Configuration. `[server]`/`[database]`/`[auth]` go through the layered
//! `config` crate loader (`config_loader::load_layered`): `config/default.toml`
//! -> `config/{APP_ENV}.toml` -> env (`APP__SECTION__KEY`) -> `config/local.toml`
//! (gitignored). Everything else is still a plain env var (a `.env` file is
//! loaded first if present) -- see `.env.example` for every variable, and
//! `docs/architecture.md` for the legacy-env compatibility this module
//! carries (`AUTH_MODE`, `CONTROL_DATABASE_URL`, `BIND_ADDR`,
//! `SUPABASE_URL`/`SUPABASE_JWT_AUD`/`SUPABASE_JWT_SECRET`).

use std::fmt;
use std::net::IpAddr;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const DEFAULT_XAI_BASE_URL: &str = "https://api.x.ai/v1";
const DEFAULT_BUCKET_DIR: &str = "data/bucket";
const DEFAULT_LEAD_MIN_FIT_SCORE: i64 = 50;
const DEFAULT_CRAWL_MAX_PAGES: u32 = 4;
const DEFAULT_CRAWL_TIMEOUT_SECS: u64 = 10;
const DEFAULT_CRAWL_MAX_BYTES: u64 = 1_000_000;
const DEFAULT_DATA_DIR: &str = "data";
const DEFAULT_QUERY_MAX_ROWS: i64 = 1000;
const DEFAULT_QUERY_TIMEOUT_MS: u64 = 5000;
const DEFAULT_CHAT_MAX_TOOL_ROUNDS: u32 = 6;
const DEFAULT_CHAT_MAX_TOKENS: u32 = 1024;
const DEFAULT_CHAT_MAX_WRITES_PER_TURN: u32 = 5;
/// `config/dev.toml`'s committed HS256 dev secret. Refused outside
/// `Env::Dev` the same way Desde0's committed Plaid dev key is -- a
/// deployment that inherited it by accident must fail loudly, not verify
/// tokens against a secret anyone can read in this repo.
const DEV_JWT_SECRET: &str = "super-secret-jwt-token-with-at-least-32-characters-long";
/// Every workspace (including the legacy single-tenant one, id
/// [`DEFAULT_WORKSPACE_ID`]) lives under `DATA_DIR/workspaces/{id}.db`; see
/// `workspace_db::WorkspaceRegistry`.
pub const DEFAULT_WORKSPACE_ID: Uuid = Uuid::nil();
/// `AuthMode::Dev`'s default user id, shared with `cargo run -- seed`'s
/// control-plane bootstrap so the dev user it creates (and the workspace it
/// owns) is exactly the one a locally-running server treats every request
/// as, with no `X-Dev-User` header needed.
pub const DEFAULT_DEV_USER_ID: Uuid = Uuid::from_u128(1);

fn default_dev_user_id() -> Uuid {
    DEFAULT_DEV_USER_ID
}

/// Which deployment environment this process is running as, decided by
/// `APP_ENV` (`dev` when unset). Gates the production refusals below
/// (`Config::validate`): unrecognized is treated as [`Env::Prod`], the
/// fail-safe direction -- an unrecognized value must never land a dev-mode
/// or dev-secret bypass in whatever that environment turns out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Env {
    Dev,
    Staging,
    Prod,
}

impl Env {
    fn parse(raw: &str) -> anyhow::Result<Self> {
        match raw {
            "dev" => Ok(Self::Dev),
            "staging" => Ok(Self::Staging),
            "prod" => Ok(Self::Prod),
            other => bail!("unknown APP_ENV `{other}`; expected `dev`, `staging` or `prod`"),
        }
    }
}

/// The subset of config that goes through the layered `config` crate loader
/// (`config_loader::load_layered`). See plan B1: only these three sections;
/// everything else in [`Config`] stays a plain env var for now.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Layered {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    #[serde(default)]
    pub auth: AuthConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    pub host: IpAddr,
    pub port: u16,
    /// Comma-separated CORS allowlist; see [`ServerConfig::cors_origins_list`].
    #[serde(default)]
    pub cors_origins: String,
}

impl ServerConfig {
    pub fn cors_origins_list(&self) -> Vec<String> {
        self.cors_origins
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DatabaseConfig {
    /// The control-plane Postgres URL. Empty disables workspace-membership
    /// enforcement -- see [`Config::control_database_url`].
    #[serde(default)]
    pub url: String,
}

/// Where the token-signing key material comes from, or `dev` to bypass
/// verification entirely. `Hs256` is a shared secret (local dev and tests:
/// no network, no identity provider); `Jwks` fetches public keys from a
/// JWKS URL (any OIDC provider, Supabase included); `Hybrid` configures
/// both and dispatches per token by the JWT header `alg`, so a locally
/// minted HS256 token and a provider-issued ES256 token both validate
/// against one running server. `Dev` treats every request as
/// [`AuthConfig::dev_user_id`] (overridable with `X-Dev-User`); refused at
/// boot outside [`Env::Dev`].
#[derive(Debug, Clone, Copy, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    Hs256,
    #[default]
    Jwks,
    Hybrid,
    Dev,
}

/// The identity provider, expressed entirely as configuration -- no
/// provider name anywhere in the verifier itself; see `crate::auth::oidc`.
#[derive(Clone, Deserialize, Serialize)]
pub struct AuthConfig {
    #[serde(default)]
    pub mode: AuthMode,
    /// JWKS URL. Required when `mode` is `jwks` or (together with
    /// `jwt_secret`) `hybrid`.
    #[serde(default)]
    pub jwks_url: Option<String>,
    /// Shared secret. Required when `mode` is `hs256` or (together with
    /// `jwks_url`) `hybrid`. Local dev and tests only.
    #[serde(default)]
    pub jwt_secret: Option<String>,
    /// Expected issuer (`iss` claim).
    #[serde(default)]
    pub issuer: String,
    /// Expected audience (`aud` claim).
    #[serde(default)]
    pub audience: String,
    /// `mode = "dev"`'s fixed user id, overridable per-request with
    /// `X-Dev-User: <uuid>`.
    #[serde(default = "default_dev_user_id")]
    pub dev_user_id: Uuid,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            mode: AuthMode::Jwks,
            jwks_url: None,
            jwt_secret: None,
            issuer: String::new(),
            audience: String::new(),
            dev_user_id: DEFAULT_DEV_USER_ID,
        }
    }
}

// Hand-written so `jwt_secret` cannot reach a log through a `{config:?}`.
impl fmt::Debug for AuthConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthConfig")
            .field("mode", &self.mode)
            .field("jwks_url", &self.jwks_url)
            .field("jwt_secret", &redacted(self.jwt_secret.as_ref()))
            .field("issuer", &self.issuer)
            .field("audience", &self.audience)
            .field("dev_user_id", &self.dev_user_id)
            .finish()
    }
}

/// Say whether a secret is set without saying what it is.
fn redacted(v: Option<&String>) -> &'static str {
    if v.is_some_and(|s| !s.is_empty()) {
        "<set>"
    } else {
        "<unset>"
    }
}

#[derive(Clone)]
pub struct Config {
    pub env: Env,
    pub server: ServerConfig,
    pub auth: AuthConfig,
    /// `{server.host}:{server.port}`, or the legacy `BIND_ADDR` env var
    /// when set (deprecated; see `Config::from_env`).
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
    /// The control-plane Postgres URL, resolved from `[database] url`
    /// (`APP__DATABASE__URL` / the bare `DATABASE_URL` env var, both
    /// canonical) and the legacy `CONTROL_DATABASE_URL` env var (deprecated,
    /// wins when set). `None` disables workspace-membership enforcement
    /// (every workspace id is accessible as `owner`) -- see
    /// `workspace_db::scope`.
    pub control_database_url: Option<String>,
    /// Per-workspace SQLite files live under `{data_dir}/workspaces/{id}.db`,
    /// including the legacy default workspace ([`DEFAULT_WORKSPACE_ID`]).
    pub data_dir: String,
    /// Hard cap on rows returned by `POST /w/{ws}/queries/{id}/run`.
    pub query_max_rows: i64,
    pub query_timeout_ms: u64,
    pub chat: ChatConfig,
}

#[derive(Clone)]
pub struct ChatConfig {
    pub provider: ChatProviderConfig,
    /// Model-call <-> tool-call round trips per turn, before the assistant
    /// must answer with a final message.
    pub max_tool_rounds: u32,
    /// `max_tokens` passed to the provider on each model call.
    pub max_tokens: u32,
    /// Data-mode writes (action-tool calls) allowed per turn; exceeding it
    /// is a tool error, never a silent stop -- see `crate::chat::tools`.
    pub max_writes_per_turn: u32,
    /// USD per 1M prompt/completion tokens, for recording `cost_usd` on
    /// assistant messages. `None` leaves `cost_usd` null rather than
    /// guessing a price.
    pub price_input_per_1m_usd: Option<f64>,
    pub price_output_per_1m_usd: Option<f64>,
}

#[derive(Clone)]
pub enum ChatProviderConfig {
    /// Deterministic, no network; exercises the real tool-calling loop in
    /// both modes. The default when `XAI_API_KEY` is unset.
    Fake,
    /// xAI Grok via the OpenAI-compatible chat completions API, with
    /// streaming.
    Grok {
        api_key: String,
        model: String,
        base_url: String,
    },
}

impl fmt::Debug for ChatProviderConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fake => f.write_str("Fake"),
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
            .field("env", &self.env)
            .field("bind_addr", &self.bind_addr)
            .field("auth", &self.auth)
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
            .field("query_max_rows", &self.query_max_rows)
            .field("query_timeout_ms", &self.query_timeout_ms)
            .field("chat_provider", &self.chat.provider)
            .field("cors_origins", &self.server.cors_origins_list())
            .finish()
    }
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let env = Env::parse(&crate::config_loader::app_env())?;
        let layered = crate::config_loader::load_layered().context("loading config")?;
        let (server, auth) = apply_legacy_aliases(layered.server, layered.auth)?;
        let control_database_url = resolve_control_database_url(layered.database.url);
        let bind_addr = resolve_bind_addr(&server);

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
        let data_dir = var("DATA_DIR").unwrap_or_else(|| DEFAULT_DATA_DIR.to_owned());
        let query_max_rows = var_parsed("QUERY_MAX_ROWS", DEFAULT_QUERY_MAX_ROWS)?;
        let query_timeout_ms = var_parsed("QUERY_TIMEOUT_MS", DEFAULT_QUERY_TIMEOUT_MS)?;
        let chat = chat_config()?;

        let config = Self {
            env,
            server,
            auth,
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
            query_max_rows,
            query_timeout_ms,
            chat,
        };
        config.validate()?;
        Ok(config)
    }

    /// Refusals that only make sense once every field is loaded: dev mode
    /// and the committed dev HS256 secret are both refused outside
    /// `Env::Dev`, so a config that silently inherited either cannot run in
    /// production -- the same "fail at boot, not at the first request"
    /// rule `auth::oidc::OidcVerifier::new` already follows for a missing
    /// key.
    fn validate(&self) -> anyhow::Result<()> {
        if self.env == Env::Dev {
            return Ok(());
        }
        if self.auth.mode == AuthMode::Dev {
            bail!("auth.mode = dev is not allowed outside Env::Dev (APP_ENV=dev)");
        }
        if matches!(self.auth.mode, AuthMode::Hs256 | AuthMode::Hybrid)
            && self.auth.jwt_secret.as_deref() == Some(DEV_JWT_SECRET)
        {
            bail!(
                "auth.jwt_secret is config/dev.toml's committed dev secret; refusing to start \
                 outside Env::Dev (APP_ENV=dev)"
            );
        }
        Ok(())
    }
}

/// Legacy env vars, mapped onto the new config with a startup warning. Kept
/// separate from `[auth]`'s own fields so the layered loader's precedence
/// (env overrides file, local.toml overrides env) stays legible; these are
/// a SECOND pass applied after that loader runs.
fn apply_legacy_aliases(
    server: ServerConfig,
    mut auth: AuthConfig,
) -> anyhow::Result<(ServerConfig, AuthConfig)> {
    reject_dropped_supabase_env()?;

    if let Some(mode) = var("AUTH_MODE") {
        tracing::warn!(
            legacy = "AUTH_MODE",
            replacement = "APP__AUTH__MODE",
            "AUTH_MODE is deprecated; set APP__AUTH__MODE instead"
        );
        auth.mode = match mode.as_str() {
            "dev" => AuthMode::Dev,
            "supabase" => AuthMode::Jwks,
            other => bail!("AUTH_MODE must be `dev` or `supabase`, got `{other}`"),
        };
    }
    Ok((server, auth))
}

/// `SUPABASE_URL`/`SUPABASE_JWT_AUD`/`SUPABASE_JWT_SECRET` are no longer
/// read. Failing loudly (rather than silently ignoring them) is the point:
/// a deployment that set one and nothing else would otherwise boot into
/// `auth.mode = jwks` with an empty issuer, rejecting every token with no
/// hint why.
fn reject_dropped_supabase_env() -> anyhow::Result<()> {
    let checks: &[(&str, &[&str])] = &[
        (
            "SUPABASE_URL",
            &["APP__AUTH__JWKS_URL", "APP__AUTH__ISSUER"],
        ),
        ("SUPABASE_JWT_AUD", &["APP__AUTH__AUDIENCE"]),
        ("SUPABASE_JWT_SECRET", &["APP__AUTH__JWT_SECRET"]),
    ];
    for (legacy, replacements) in checks {
        if var(legacy).is_some() && !replacements.iter().any(|r| var(r).is_some()) {
            bail!(
                "{legacy} is no longer read; set {} instead (see .env.example)",
                replacements.join(" / ")
            );
        }
    }
    Ok(())
}

/// The control-plane Postgres URL: `[database] url` (`APP__DATABASE__URL`,
/// or the bare `DATABASE_URL` env var when it looks like a Postgres URL --
/// both canonical, matching what infra's PR #18 injects), overridden by the
/// legacy `CONTROL_DATABASE_URL` env var when set. A `DATABASE_URL` that
/// looks like a `sqlite:` path is the OLD meaning (the legacy default
/// workspace's data file, now `DATA_DIR`-only per plan B1) and is ignored
/// with a warning rather than misread as a control-plane url.
fn resolve_control_database_url(layered_url: String) -> Option<String> {
    let mut url = non_empty(Some(layered_url));

    if let Some(raw) = var("DATABASE_URL") {
        if raw.starts_with("postgres://") || raw.starts_with("postgresql://") {
            url = Some(raw);
        } else {
            tracing::warn!(
                "DATABASE_URL is set to a non-postgres url (the old per-workspace sqlite \
                 path); ignoring it for the control plane, which now reads DATABASE_URL (or \
                 APP__DATABASE__URL) only as a postgres:// url. The legacy default workspace's \
                 data now lives under DATA_DIR regardless of this setting."
            );
        }
    }
    if let Some(legacy) = var("CONTROL_DATABASE_URL") {
        tracing::warn!(
            legacy = "CONTROL_DATABASE_URL",
            replacement = "DATABASE_URL or APP__DATABASE__URL",
            "CONTROL_DATABASE_URL is deprecated; set DATABASE_URL or APP__DATABASE__URL instead"
        );
        url = Some(legacy);
    }
    url
}

fn resolve_bind_addr(server: &ServerConfig) -> String {
    var("BIND_ADDR").map_or_else(
        || format!("{}:{}", server.host, server.port),
        |legacy| {
            tracing::warn!(
                legacy = "BIND_ADDR",
                replacement = "APP__SERVER__HOST / APP__SERVER__PORT",
                "BIND_ADDR is deprecated; set APP__SERVER__HOST and APP__SERVER__PORT instead"
            );
            legacy
        },
    )
}

/// `CHAT_PROVIDER=grok|fake`, defaulting to `fake` unless `XAI_API_KEY` is
/// set (in which case it defaults to `grok`) -- so a box with no xAI key
/// never silently tries to reach the network, but a configured box doesn't
/// need a second env var to turn chat on.
fn chat_config() -> anyhow::Result<ChatConfig> {
    let xai_api_key = var("XAI_API_KEY");
    let provider = match var("CHAT_PROVIDER").as_deref() {
        None => match xai_api_key {
            Some(api_key) => ChatProviderConfig::Grok {
                api_key,
                model: var("XAI_CHAT_MODEL")
                    .or_else(|| var("XAI_MODEL"))
                    .context("CHAT_PROVIDER=grok requires XAI_CHAT_MODEL or XAI_MODEL")?,
                base_url: var("XAI_BASE_URL").unwrap_or_else(|| DEFAULT_XAI_BASE_URL.to_owned()),
            },
            None => ChatProviderConfig::Fake,
        },
        Some("fake") => ChatProviderConfig::Fake,
        Some("grok") => ChatProviderConfig::Grok {
            api_key: xai_api_key.context("CHAT_PROVIDER=grok requires XAI_API_KEY")?,
            model: var("XAI_CHAT_MODEL")
                .or_else(|| var("XAI_MODEL"))
                .context("CHAT_PROVIDER=grok requires XAI_CHAT_MODEL or XAI_MODEL")?,
            base_url: var("XAI_BASE_URL").unwrap_or_else(|| DEFAULT_XAI_BASE_URL.to_owned()),
        },
        Some(other) => bail!("CHAT_PROVIDER must be `fake` or `grok`, got `{other}`"),
    };
    Ok(ChatConfig {
        provider,
        max_tool_rounds: var_parsed("CHAT_MAX_TOOL_ROUNDS", DEFAULT_CHAT_MAX_TOOL_ROUNDS)?,
        max_tokens: var_parsed("CHAT_MAX_TOKENS", DEFAULT_CHAT_MAX_TOKENS)?,
        max_writes_per_turn: var_parsed(
            "CHAT_MAX_WRITES_PER_TURN",
            DEFAULT_CHAT_MAX_WRITES_PER_TURN,
        )?,
        price_input_per_1m_usd: var("CHAT_PRICE_INPUT_PER_1M_USD")
            .map(|v| v.parse())
            .transpose()
            .context("CHAT_PRICE_INPUT_PER_1M_USD is invalid")?,
        price_output_per_1m_usd: var("CHAT_PRICE_OUTPUT_PER_1M_USD")
            .map(|v| v.parse())
            .transpose()
            .context("CHAT_PRICE_OUTPUT_PER_1M_USD is invalid")?,
    })
}

/// A set, non-empty environment variable.
fn var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// Present-but-empty is treated as absent, the same rule `var` follows for
/// env vars -- a `[database] url = ""` default.toml value must read the
/// same as the key being unset.
fn non_empty(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
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
