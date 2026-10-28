//! `wake` mode: cold-wake latency for a brand-new database (create +
//! migrate + spawn) vs. an existing, seeded one (open + migration check +
//! spawn), plus the first command's latency right after each wake.

use std::future::Future;
use std::time::Instant;

use lula_dog_backend::actor::{ActorConfig, WorkspaceActor};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::common::{db_path, elapsed_us, seed, sqlite_url, stats_from};

const REPS: usize = 50;
const OPPORTUNITIES: usize = 200;
const LEADS: usize = 200;

async fn wake_new() -> anyhow::Result<(u64, u64)> {
    let dir = tempfile::tempdir()?;
    let path = db_path(dir.path(), "wake-new");
    let start = Instant::now();
    let handle = WorkspaceActor::spawn(&path, ActorConfig::default()).await?;
    let wake_us = elapsed_us(start);

    let first_start = Instant::now();
    let reply = handle
        .send(
            Uuid::new_v4(),
            "opportunity.create",
            json!({"company": "First"}),
        )
        .await;
    let first_us = elapsed_us(first_start);
    reply?;
    Ok((wake_us, first_us))
}

async fn wake_existing() -> anyhow::Result<(u64, u64)> {
    let dir = tempfile::tempdir()?;
    let path = db_path(dir.path(), "wake-existing");
    let ids = seed(&sqlite_url(&path), OPPORTUNITIES, LEADS).await?;
    let opportunity_id = ids.opportunity_ids.first().copied().unwrap_or(1);

    let start = Instant::now();
    let handle = WorkspaceActor::spawn(&path, ActorConfig::default()).await?;
    let wake_us = elapsed_us(start);

    let first_start = Instant::now();
    let reply = handle
        .send(
            Uuid::new_v4(),
            "task.create",
            json!({"opportunity_id": opportunity_id, "title": "First after wake"}),
        )
        .await;
    let first_us = elapsed_us(first_start);
    reply?;
    Ok((wake_us, first_us))
}

async fn measure<F, Fut>(label: &str, one: F) -> anyhow::Result<Value>
where
    F: Fn() -> Fut,
    Fut: Future<Output = anyhow::Result<(u64, u64)>>,
{
    let mut wakes = Vec::with_capacity(REPS);
    let mut firsts = Vec::with_capacity(REPS);
    let mut errors = 0usize;
    for _ in 0..REPS {
        match one().await {
            Ok((wake_us, first_us)) => {
                wakes.push(wake_us);
                firsts.push(first_us);
            }
            Err(_) => errors += 1,
        }
    }
    let wake_stats = stats_from(wakes, errors);
    let first_stats = stats_from(firsts, errors);
    println!(
        "  {label:<10} wake p50={:>6}us p95={:>6}us | first-command p50={:>6}us p95={:>6}us \
         (n={REPS}, errors={errors})",
        wake_stats.p50_us, wake_stats.p95_us, first_stats.p50_us, first_stats.p95_us,
    );
    Ok(json!({
        "label": label,
        "reps": REPS,
        "wake": wake_stats,
        "first_command": first_stats,
    }))
}

pub async fn run() -> anyhow::Result<Value> {
    println!("== wake ==");
    let new_db = measure("new_db", wake_new).await?;
    let existing_db = measure("existing_db", wake_existing).await?;
    Ok(json!({
        "new_db": new_db,
        "existing_db": existing_db,
    }))
}
