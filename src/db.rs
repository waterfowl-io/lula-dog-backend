//! SQLite pool setup. Migrations are embedded at compile time and applied on
//! every startup.

use std::str::FromStr;
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

/// The connection options every workspace SQLite file is opened with,
/// whether through a pool (here) or the workspace actor's single connection
/// (`crate::actor`) -- WAL, `synchronous = NORMAL`, foreign keys on, a busy
/// timeout for the rare lock wait.
pub fn options(database_url: &str) -> anyhow::Result<SqliteConnectOptions> {
    Ok(SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5)))
}

pub async fn connect(database_url: &str) -> anyhow::Result<SqlitePool> {
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(options(database_url)?)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}
