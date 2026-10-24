//! `lula-dog-backend [serve|seed|import ...]`. `serve` is the default.

use std::path::{Path, PathBuf};

use anyhow::bail;
use lula_dog_backend::bucket::Bucket;
use lula_dog_backend::config::Config;
use lula_dog_backend::importers::bizapedia::{self, ColumnMapping};
use lula_dog_backend::{AppState, db, extractor_from_config, router, seed, target_profiles};
use tracing_subscriber::EnvFilter;

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
        Some("seed") => seed_cmd(config).await,
        Some("import") => import_cmd(config, args.get(1..).unwrap_or_default()).await,
        Some(other) => bail!("unknown command `{other}`; expected serve, seed or import"),
    }
}

async fn serve(config: Config) -> anyhow::Result<()> {
    tracing::info!(?config, "starting");
    let pool = db::connect(&config.database_url).await?;
    let extractor = extractor_from_config(&config.extractor)?;
    let app = router(AppState { pool, extractor });
    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    tracing::info!(addr = %listener.local_addr()?, "listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

async fn seed_cmd(config: Config) -> anyhow::Result<()> {
    let pool = db::connect(&config.database_url).await?;
    let summary = seed::run(&pool).await?;
    let mut conn = pool.acquire().await?;
    target_profiles::seed_default(&mut conn, config.lead_min_fit_score).await?;
    tracing::info!(
        opportunities = summary.opportunities,
        database = %config.database_url,
        "seeded"
    );
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

async fn import_cmd(config: Config, args: &[String]) -> anyhow::Result<()> {
    let flags = parse_flags(args.get(1..).unwrap_or_default());
    match args.first().map(String::as_str) {
        Some("bizapedia") => import_bizapedia(config, &flags).await,
        Some(other) => bail!("unknown import source `{other}`; expected `bizapedia`"),
        None => bail!("usage: import bizapedia <path.csv> [--mapping <file>]"),
    }
}

async fn import_bizapedia(config: Config, flags: &Flags<'_>) -> anyhow::Result<()> {
    let Some(&csv_path) = flags.positional.first() else {
        bail!("usage: import bizapedia <path.csv> [--mapping <file>]");
    };
    let mapping_path: Option<PathBuf> = flags.named.get("mapping").map(PathBuf::from);
    let mapping = ColumnMapping::load(mapping_path.as_deref())?;

    let pool = db::connect(&config.database_url).await?;
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
