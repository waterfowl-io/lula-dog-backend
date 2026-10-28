//! `latency` mode: direct dispatch vs. the actor vs. the actor with
//! group commit, at concurrency 1 and 50, plus a 50-writer/50-reader mix.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use lula_dog_backend::actor::{ActorConfig, WorkspaceActor};
use serde_json::{Value, json};

use crate::common::{
    FreshDb, Target, elapsed_us, fresh_db, is_busy_or_locked, run_concurrent, sqlite_url,
    stats_from,
};

const WARMUP: usize = 200;
const N: usize = 5_000;
const OPPORTUNITIES: usize = 200;
const LEADS: usize = 2_000;

struct Path {
    name: &'static str,
    group_commit: bool,
}

const PATHS_ACTOR: [Path; 2] = [
    Path {
        name: "actor",
        group_commit: false,
    },
    Path {
        name: "actor_group_commit",
        group_commit: true,
    },
];

async fn target_for(db: &FreshDb, path: &str, group_commit: bool) -> anyhow::Result<Target> {
    if path == "direct" {
        let pool = lula_dog_backend::db::connect(&sqlite_url(&db.path)).await?;
        return Ok(Target::Direct(pool));
    }
    let config = ActorConfig {
        group_commit,
        ..ActorConfig::default()
    };
    let handle = WorkspaceActor::spawn(&db.path, config).await?;
    Ok(Target::Actor(handle))
}

async fn one_measurement(
    path: &str,
    group_commit: bool,
    concurrency: usize,
) -> anyhow::Result<Value> {
    let db = fresh_db(OPPORTUNITIES, LEADS).await?;
    let target = target_for(&db, path, group_commit).await?;
    let start = Instant::now();
    let (samples, errors) = run_concurrent(target, db.seed.clone(), WARMUP, N, concurrency).await;
    let wall = start.elapsed();
    let busy = errors.iter().filter(|e| is_busy_or_locked(e)).count();
    let stats = stats_from(samples, errors.len());
    // `stats.count` is at most `WARMUP + N` (a few thousand), nowhere near
    // f64's exact-integer range, so the conversion loses nothing in
    // practice.
    #[allow(clippy::cast_precision_loss)]
    let count_f64 = stats.count as f64;
    let throughput = if wall.as_secs_f64() > 0.0 {
        count_f64 / wall.as_secs_f64()
    } else {
        0.0
    };
    println!(
        "  {path:<20} concurrency={concurrency:<3} p50={:>6}us p95={:>6}us p99={:>6}us \
         max={:>7}us wall={:>7.2?} throughput={throughput:>8.1} cmd/s errors={} (busy/locked={busy})",
        stats.p50_us,
        stats.p95_us,
        stats.p99_us,
        stats.max_us,
        wall,
        errors.len(),
    );
    Ok(json!({
        "path": path,
        "concurrency": concurrency,
        "stats": stats,
        "wall_ms": u64::try_from(wall.as_millis()).unwrap_or(u64::MAX),
        "throughput_cmd_per_s": throughput,
        "busy_or_locked_errors": busy,
    }))
}

/// 50 writers (the given target/path) racing 50 readers on a separate
/// read pool, to show reads aren't blocked by the write path.
async fn mixed_measurement(path: &str, group_commit: bool) -> anyhow::Result<Value> {
    let db = fresh_db(OPPORTUNITIES, LEADS).await?;
    let target = target_for(&db, path, group_commit).await?;
    let read_pool = lula_dog_backend::db::connect(&sqlite_url(&db.path)).await?;

    let done = Arc::new(AtomicBool::new(false));
    let mut reader_tasks = Vec::with_capacity(50);
    for _ in 0..50 {
        let read_pool = read_pool.clone();
        let done = done.clone();
        reader_tasks.push(tokio::spawn(async move {
            let mut samples = Vec::new();
            while !done.load(Ordering::Relaxed) {
                let start = Instant::now();
                let rows: Result<Vec<(i64,)>, sqlx::Error> =
                    sqlx::query_as("SELECT id FROM opportunities LIMIT 50")
                        .fetch_all(&read_pool)
                        .await;
                if rows.is_ok() {
                    samples.push(elapsed_us(start));
                }
                tokio::task::yield_now().await;
            }
            samples
        }));
    }

    let write_start = Instant::now();
    let (write_samples, write_errors) =
        run_concurrent(target, db.seed.clone(), WARMUP, N, 50).await;
    let write_wall = write_start.elapsed();
    done.store(true, Ordering::Relaxed);

    let mut read_samples = Vec::new();
    for task in reader_tasks {
        if let Ok(samples) = task.await {
            read_samples.extend(samples);
        }
    }

    let write_stats = stats_from(write_samples, write_errors.len());
    let read_stats = stats_from(read_samples, 0);
    println!(
        "  {path:<20} mixed(50w+50r)   writes p50={:>6}us p99={:>6}us wall={:>7.2?} | \
         reads n={:<6} p50={:>6}us p99={:>6}us",
        write_stats.p50_us,
        write_stats.p99_us,
        write_wall,
        read_stats.count,
        read_stats.p50_us,
        read_stats.p99_us,
    );
    Ok(json!({
        "path": path,
        "writes": write_stats,
        "reads": read_stats,
    }))
}

pub async fn run() -> anyhow::Result<Value> {
    println!("== latency ==");
    let mut measurements = Vec::new();

    for concurrency in [1usize, 50] {
        measurements.push(one_measurement("direct", false, concurrency).await?);
    }
    for path in &PATHS_ACTOR {
        for concurrency in [1usize, 50] {
            measurements.push(one_measurement(path.name, path.group_commit, concurrency).await?);
        }
    }

    println!("  -- mixed reads+writes at concurrency 50 --");
    let mut mixed = Vec::new();
    mixed.push(mixed_measurement("direct", false).await?);
    for path in &PATHS_ACTOR {
        mixed.push(mixed_measurement(path.name, path.group_commit).await?);
    }

    Ok(json!({
        "warmup": WARMUP,
        "n": N,
        "measurements": measurements,
        "mixed_reads_and_writes": mixed,
    }))
}
