//! `lula-dog-backend [serve]`. `serve` is the default.

use anyhow::bail;
use lula_dog_backend::config::Config;
use lula_dog_backend::{AppState, db, extractor_from_config, router};
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
    let command = std::env::args().nth(1);
    match command.as_deref() {
        None | Some("serve") => serve(config).await,
        Some(other) => bail!("unknown command `{other}`; expected `serve`"),
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
