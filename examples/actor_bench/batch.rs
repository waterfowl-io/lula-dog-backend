//! `batch` mode: runner-style `send_batch` calls of 100/500 `task.create`
//! commands racing 10 interactive callers, with vs. without the batch
//! load, to see how much latency a big ingest batch adds to a user click.

use std::sync::Arc;
use std::time::Instant;

use lula_dog_backend::actor::{ActorConfig, WorkspaceActor};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::common::{SeedIds, Target, elapsed_us, fresh_db, run_concurrent, stats_from};

const OPPORTUNITIES: usize = 200;
const INTERACTIVE_WARMUP: usize = 50;
const INTERACTIVE_N: usize = 500;
const INTERACTIVE_CONCURRENCY: usize = 10;
const NUM_BATCHES: usize = 10;
/// Lead candidates each interactive run consumes (every 5th command is a
/// `lead.accept`, and each lead can only be accepted once).
const LEADS_PER_RUN: usize = (INTERACTIVE_WARMUP + INTERACTIVE_N) / 5 + 1;

async fn run_batches(
    handle: lula_dog_backend::actor::WorkspaceHandle,
    opportunity_id: i64,
    batch_size: usize,
    num_batches: usize,
) -> (Vec<u64>, usize) {
    let mut errors = 0;
    let mut times = Vec::with_capacity(num_batches);
    for b in 0..num_batches {
        let commands: Vec<(String, Value)> = (0..batch_size)
            .map(|i| {
                (
                    "task.create".to_owned(),
                    json!({
                        "opportunity_id": opportunity_id,
                        "title": format!("Runner batch {b} item {i}"),
                    }),
                )
            })
            .collect();
        let start = Instant::now();
        match handle.send_batch(Uuid::new_v4(), commands).await {
            Ok(_) => times.push(elapsed_us(start)),
            Err(_) => errors += 1,
        }
    }
    (times, errors)
}

async fn one_batch_size(batch_size: usize) -> anyhow::Result<Value> {
    let db = fresh_db(OPPORTUNITIES, 2 * LEADS_PER_RUN).await?;
    let handle = WorkspaceActor::spawn(&db.path, ActorConfig::default()).await?;
    let opportunity_id = db.seed.opportunity_ids.first().copied().unwrap_or(1);

    // Split the seeded leads so the loaded run doesn't re-accept leads the
    // baseline run already accepted.
    let (first_leads, second_leads) = db.seed.lead_ids.split_at(LEADS_PER_RUN);
    let baseline_seed = Arc::new(SeedIds {
        opportunity_ids: db.seed.opportunity_ids.clone(),
        lead_ids: first_leads.to_vec(),
    });
    let loaded_seed = Arc::new(SeedIds {
        opportunity_ids: db.seed.opportunity_ids.clone(),
        lead_ids: second_leads.to_vec(),
    });

    let baseline_target = Target::Actor(handle.clone());
    let (baseline_samples, baseline_errors) = run_concurrent(
        baseline_target,
        baseline_seed,
        INTERACTIVE_WARMUP,
        INTERACTIVE_N,
        INTERACTIVE_CONCURRENCY,
    )
    .await;
    let baseline_stats = stats_from(baseline_samples, baseline_errors.len());

    let loaded_target = Target::Actor(handle.clone());
    let seed = loaded_seed;
    let interactive_fut = run_concurrent(
        loaded_target,
        seed,
        INTERACTIVE_WARMUP,
        INTERACTIVE_N,
        INTERACTIVE_CONCURRENCY,
    );
    let batch_fut = run_batches(handle.clone(), opportunity_id, batch_size, NUM_BATCHES);
    let ((loaded_samples, loaded_errors), (batch_times, batch_errors)) =
        tokio::join!(interactive_fut, batch_fut);
    let loaded_stats = stats_from(loaded_samples, loaded_errors.len());
    let batch_stats = stats_from(batch_times, batch_errors);

    println!(
        "  batch_size={batch_size:<4} batch_tx p50={:>7}us p95={:>7}us | \
         interactive p50 {:>6}us->{:>6}us  p99 {:>6}us->{:>6}us (baseline -> with batch load)",
        batch_stats.p50_us,
        batch_stats.p95_us,
        baseline_stats.p50_us,
        loaded_stats.p50_us,
        baseline_stats.p99_us,
        loaded_stats.p99_us,
    );

    Ok(json!({
        "batch_size": batch_size,
        "num_batches": NUM_BATCHES,
        "batch_tx": batch_stats,
        "interactive_without_batch_load": baseline_stats,
        "interactive_with_batch_load": loaded_stats,
    }))
}

pub async fn run() -> anyhow::Result<Value> {
    println!("== batch ==");
    let mut by_size = Vec::new();
    for batch_size in [100usize, 500] {
        by_size.push(one_batch_size(batch_size).await?);
    }
    Ok(json!({ "batch_sizes": by_size }))
}
