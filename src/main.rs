//! `lula-dog-backend [serve|seed|import ...]`. `serve` is the default.
//!
//! `seed`, `import`, `enrich` and `classify` all operate on one workspace's
//! SQLite file, chosen with `--workspace <uuid>` (default: the legacy
//! single-tenant workspace that reads `DATABASE_URL` directly -- see
//! `config::DEFAULT_WORKSPACE_ID`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::bail;
use lula_dog_backend::auth::Auth;
use lula_dog_backend::bucket::Bucket;
use lula_dog_backend::config::{self, Config};
use lula_dog_backend::importers::bizapedia::{self, ColumnMapping};
use lula_dog_backend::importers::il_sos_llc;
use lula_dog_backend::workspace_db::WorkspaceRegistry;
use lula_dog_backend::{
    AppState, classifier_from_config, control, enrichment, extractor_from_config,
    fetcher_from_config, places_from_config, router, seed, target_profiles,
};
use sqlx::SqlitePool;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // A missing .env is fine; the environment may already be set.
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("serve") => serve(config).await,
        Some("seed") => seed_cmd(config, parse_flags(args.get(1..).unwrap_or_default())).await,
        Some("import") => import_cmd(config, args.get(1..).unwrap_or_default()).await,
        Some("enrich") => enrich_cmd(config, parse_flags(args.get(1..).unwrap_or_default())).await,
        Some("classify") => {
            classify_cmd(config, parse_flags(args.get(1..).unwrap_or_default())).await
        }
        Some(other) => {
            bail!("unknown command `{other}`; expected serve, seed, import, enrich or classify")
        }
    }
}

async fn serve(config: Config) -> anyhow::Result<()> {
    tracing::info!(?config, "starting");
    let control = if let Some(url) = &config.control_database_url {
        Some(control::connect(url).await?)
    } else {
        tracing::warn!(
            "CONTROL_DATABASE_URL is not set: workspace-membership checks are DISABLED \
             (every workspace id is accessible as `owner`). Fine for local/single-tenant \
             use; do not run this in production without a control plane."
        );
        None
    };
    let extractor = extractor_from_config(&config.extractor)?;
    let auth = Arc::new(Auth::from_config(&config.auth)?);
    let workspaces = WorkspaceRegistry::new(config.data_dir.clone());
    let chat_provider =
        lula_dog_backend::chat::provider::provider_from_config(&config.chat.provider)?;
    let build_agent =
        lula_dog_backend::chat::build::agent_from_config(chat_provider.clone(), &config.chat);
    let app = router(AppState {
        extractor,
        control,
        workspaces,
        auth,
        query_max_rows: config.query_max_rows,
        query_timeout_ms: config.query_timeout_ms,
        chat: config.chat.clone(),
        chat_provider,
        build_agent,
        cors_origins: config.server.cors_origins_list(),
    });
    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    tracing::info!(addr = %listener.local_addr()?, "listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

/// Positional arguments and `--flag value` pairs, split from one argument
/// list. Every `--xxx` token consumes the following token as its value.
struct Flags<'a> {
    positional: Vec<&'a str>,
    named: std::collections::HashMap<&'a str, &'a str>,
}

fn parse_flags(args: &[String]) -> Flags<'_> {
    let mut positional = Vec::new();
    let mut named = std::collections::HashMap::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if let Some(flag) = arg.strip_prefix("--") {
            if let Some(value) = iter.next() {
                named.insert(flag, value.as_str());
            }
        } else {
            positional.push(arg.as_str());
        }
    }
    Flags { positional, named }
}

fn workspace_id_flag(flags: &Flags<'_>) -> anyhow::Result<Uuid> {
    flags.named.get("workspace").map_or_else(
        || Ok(config::DEFAULT_WORKSPACE_ID),
        |v| {
            v.parse()
                .map_err(|e| anyhow::anyhow!("--workspace is not a uuid: {e}"))
        },
    )
}

async fn workspace_pool(config: &Config, workspace_id: Uuid) -> anyhow::Result<SqlitePool> {
    let registry = WorkspaceRegistry::new(config.data_dir.clone());
    registry.get_or_create(workspace_id).await
}

async fn seed_cmd(config: Config, flags: Flags<'_>) -> anyhow::Result<()> {
    // An explicit --workspace seeds just that workspace's SQLite data and
    // skips the control-plane dev-user/workspace/app bootstrap below, which
    // only makes sense for the one workspace this command discovers or
    // creates on its own.
    let explicit_workspace = flags.named.contains_key("workspace");
    let control_pool = match &config.control_database_url {
        Some(url) if !explicit_workspace => Some(control::connect(url).await?),
        _ => None,
    };

    let workspace_id = if let Some(control) = &control_pool {
        seed::ensure_dev_workspace(control).await?
    } else {
        workspace_id_flag(&flags)?
    };

    let pool = workspace_pool(&config, workspace_id).await?;
    let summary = seed::run(&pool, config.lead_min_fit_score).await?;
    tracing::info!(
        opportunities = summary.opportunities,
        leads = summary.leads,
        %workspace_id,
        "seeded sqlite data"
    );

    if let Some(control) = &control_pool {
        seed::seed_control_plane(control, workspace_id).await?;
        tracing::info!(%workspace_id, "seeded control plane: workspace, saved queries, sales app");
    } else if config.control_database_url.is_some() {
        tracing::info!("--workspace given; skipped the control-plane dev-user/app bootstrap");
    } else {
        tracing::info!("CONTROL_DATABASE_URL not set; skipped the control-plane bootstrap");
    }
    Ok(())
}

async fn import_cmd(config: Config, args: &[String]) -> anyhow::Result<()> {
    let flags = parse_flags(args.get(1..).unwrap_or_default());
    match args.first().map(String::as_str) {
        Some("bizapedia") => import_bizapedia(config, &flags).await,
        Some("il-sos-llc") => import_il_sos_llc(config, &flags).await,
        Some(other) => {
            bail!("unknown import source `{other}`; expected `bizapedia` or `il-sos-llc`")
        }
        None => bail!(
            "usage: import <bizapedia|il-sos-llc> <path> [--mapping <file>] [--workspace <uuid>]"
        ),
    }
}

async fn import_bizapedia(config: Config, flags: &Flags<'_>) -> anyhow::Result<()> {
    let Some(&csv_path) = flags.positional.first() else {
        bail!("usage: import bizapedia <path.csv> [--mapping <file>] [--workspace <uuid>]");
    };
    let mapping_path: Option<PathBuf> = flags.named.get("mapping").map(PathBuf::from);
    let mapping = ColumnMapping::load(mapping_path.as_deref())?;

    let pool = workspace_pool(&config, workspace_id_flag(flags)?).await?;
    let bucket = Bucket::new(&config.bucket_dir);
    let mut conn = pool.acquire().await?;
    let profile = target_profiles::first(&mut conn).await?;
    drop(conn);

    let summary = bizapedia::import(
        &pool,
        &bucket,
        Path::new(csv_path),
        &mapping,
        profile.as_ref(),
    )
    .await?;
    tracing::info!(
        seen = summary.seen,
        new = summary.new,
        duplicate = summary.duplicate,
        filtered = summary.filtered,
        error = summary.error,
        "bizapedia import complete"
    );
    Ok(())
}

async fn import_il_sos_llc(config: Config, flags: &Flags<'_>) -> anyhow::Result<()> {
    let Some(&master_path) = flags.positional.first() else {
        bail!("usage: import il-sos-llc <path to llcallmst.txt> [--workspace <uuid>]");
    };

    let pool = workspace_pool(&config, workspace_id_flag(flags)?).await?;
    let bucket = Bucket::new(&config.bucket_dir);
    let mut conn = pool.acquire().await?;
    let profile = target_profiles::first(&mut conn).await?;
    drop(conn);

    let summary =
        il_sos_llc::import(&pool, &bucket, Path::new(master_path), profile.as_ref()).await?;
    tracing::info!(
        seen = summary.seen,
        new = summary.new,
        duplicate = summary.duplicate,
        filtered = summary.filtered,
        error = summary.error,
        "il-sos-llc import complete"
    );
    Ok(())
}

const DEFAULT_LIMIT: i64 = 50;

fn limit_flag(flags: &Flags<'_>) -> anyhow::Result<i64> {
    flags.named.get("limit").map_or_else(
        || Ok(DEFAULT_LIMIT),
        |v| {
            v.parse()
                .map_err(|e| anyhow::anyhow!("--limit is invalid: {e}"))
        },
    )
}

async fn enrich_cmd(config: Config, flags: Flags<'_>) -> anyhow::Result<()> {
    let limit = limit_flag(&flags)?;
    let pool = workspace_pool(&config, workspace_id_flag(&flags)?).await?;
    let bucket = Bucket::new(&config.bucket_dir);
    let places = places_from_config(&config.places)?;
    let fetcher = fetcher_from_config(config.crawler, &config.crawl_limits)?;

    let summary = enrichment::run_enrich(
        &pool,
        &bucket,
        &*places,
        &*fetcher,
        config.crawl_limits,
        limit,
    )
    .await?;
    tracing::info!(
        places_attempted = summary.places.attempted,
        places_success = summary.places.success,
        places_error = summary.places.error,
        crawl_attempted = summary.crawl.attempted,
        crawl_success = summary.crawl.success,
        crawl_error = summary.crawl.error,
        "enrich complete"
    );
    Ok(())
}

async fn classify_cmd(config: Config, flags: Flags<'_>) -> anyhow::Result<()> {
    let limit = limit_flag(&flags)?;
    let pool = workspace_pool(&config, workspace_id_flag(&flags)?).await?;
    let classifier = classifier_from_config(&config.classifier)?;
    let mut conn = pool.acquire().await?;
    let Some(profile) = target_profiles::first(&mut conn).await? else {
        bail!(
            "classify requires a target profile; create one with POST /target-profiles or `cargo run -- seed` first"
        );
    };
    drop(conn);

    let summary = enrichment::run_classify(&pool, &*classifier, &profile, limit).await?;
    tracing::info!(
        attempted = summary.attempted,
        success = summary.success,
        error = summary.error,
        "classify complete"
    );
    Ok(())
}
