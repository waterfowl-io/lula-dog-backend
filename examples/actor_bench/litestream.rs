//! `litestream` mode (optional): only runs if a `litestream` binary is
//! available at `/workspace/lula-actor/bin/litestream`. Seeds a larger
//! database, replicates it to a `file://` replica directory (the simple
//! single-database CLI form, no config file needed), waits for sync, then
//! times `litestream restore` a few times.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::process::Command;

use crate::common::{elapsed_us, fresh_db, stats_from};

const LITESTREAM_BIN: &str = "/workspace/lula-actor/bin/litestream";
const RESTORE_REPS: usize = 5;
const OPPORTUNITIES: usize = 2_000;
const LEADS: usize = 18_000;

fn available() -> bool {
    Path::new(LITESTREAM_BIN).is_file()
}

pub async fn run() -> anyhow::Result<Value> {
    println!("== litestream ==");
    if !available() {
        println!("  litestream binary not found at {LITESTREAM_BIN}; skipping");
        return Ok(json!({ "skipped": true, "reason": "litestream binary not found" }));
    }

    let db = fresh_db(OPPORTUNITIES, LEADS).await?;
    let file_size = tokio::fs::metadata(&db.path).await?.len();
    println!("  seeded db file size: {file_size} bytes");

    let replica_dir = db.dir.path().join("replica");
    tokio::fs::create_dir_all(&replica_dir).await?;
    let replica_url = format!("file://{}", replica_dir.display());

    let mut replicate = Command::new(LITESTREAM_BIN)
        .arg("replicate")
        .arg(&db.path)
        .arg(&replica_url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("spawning litestream replicate: {e}"))?;

    // Give the replica time to take its initial snapshot and catch up.
    tokio::time::sleep(Duration::from_secs(5)).await;

    let mut restore_times = Vec::with_capacity(RESTORE_REPS);
    let mut restore_failures = 0usize;
    for i in 0..RESTORE_REPS {
        let restore_path = db.dir.path().join(format!("restored-{i}.db"));
        let start = Instant::now();
        let status = Command::new(LITESTREAM_BIN)
            .arg("restore")
            .arg("-o")
            .arg(&restore_path)
            .arg(&replica_url)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .status()
            .await?;
        let us = elapsed_us(start);
        if status.success() && restore_path.is_file() {
            restore_times.push(us);
        } else {
            restore_failures += 1;
        }
        let _ = tokio::fs::remove_file(&restore_path).await;
    }

    let _ = replicate.kill().await;
    let _ = replicate.wait().await;

    let stats = stats_from(restore_times, restore_failures);
    println!(
        "  restore p50={:>8}us p95={:>8}us (n={}/{RESTORE_REPS}, failures={restore_failures})",
        stats.p50_us, stats.p95_us, stats.count,
    );
    Ok(json!({
        "skipped": false,
        "seeded_file_size_bytes": file_size,
        "restore": stats,
    }))
}
