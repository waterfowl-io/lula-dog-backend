//! The control plane: Supabase Postgres holding profiles, workspaces,
//! membership, app definitions, saved queries and the schema catalog.
//! Runtime queries only (no `query!` macros), so building never needs
//! `CONTROL_DATABASE_URL`. Migrations live in `migrations/control/`,
//! separate from the per-workspace SQLite migrations in `migrations/`.
//!
//! Customer data never lives here: editable data is one SQLite file per
//! workspace (see `crate::workspace_db`), runner-ingested data is DuckLake
//! (later). See `docs/architecture.md`.

pub mod app_definitions;
pub mod invites;
pub mod profiles;
pub mod saved_queries;
pub mod schema_catalog;
pub mod workspaces;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

pub type ControlPool = PgPool;

pub async fn connect(control_database_url: &str) -> anyhow::Result<ControlPool> {
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(control_database_url)
        .await?;
    sqlx::migrate!("./migrations/control").run(&pool).await?;
    Ok(pool)
}
