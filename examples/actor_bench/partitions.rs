//! `partitions` mode: key-partitioned workspaces (design §5.2).
//!
//! Workspace A gets a shared partition plus `user` partitions; workspace B
//! gets a `user` partition with the *same key* as one of A's. The bench
//! proves:
//! - isolation: each file holds only its own rows, and the same key in two
//!   workspaces is two files;
//! - atomicity within one partition: a batch with a failing command applies
//!   nothing;
//! - the hard cross-workspace invariant: the fan-out builder refuses a
//!   partition from another workspace;
//! - a cross-partition read fan-out through DuckDB (`ATTACH ... (TYPE
//!   SQLITE, READ_ONLY)`) returns exactly A's rows, can't write, and how
//!   long it takes at 2 / 50 / 200 user partitions.
//!
//! Needs a DuckDB CLI with the `sqlite_scanner` extension installed at
//! `/workspace/lula-actor/bin/duckdb`; skipped otherwise.

use std::path::Path;
use std::time::Instant;

use lula_dog_backend::actor::partition::{
    DEFAULT_FANOUT_LIMIT, PartitionError, PartitionKey, fanout_attach_sql,
};
use lula_dog_backend::actor::{ActorConfig, WorkspaceActor, WorkspaceHandle};
use serde_json::{Value, json};
use tokio::process::Command;
use uuid::Uuid;

use crate::common::{elapsed_us, sqlite_url, stats_from};

const DUCKDB_BIN: &str = "/workspace/lula-actor/bin/duckdb";
const QUERY_REPS: usize = 5;

async fn spawn_partition(data_dir: &Path, key: &PartitionKey) -> anyhow::Result<WorkspaceHandle> {
    let path = key.path(data_dir);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    WorkspaceActor::spawn(path, ActorConfig::default()).await
}

async fn create_deals(
    handle: &WorkspaceHandle,
    label: &str,
    n: usize,
    value_cents: i64,
) -> anyhow::Result<()> {
    let commands = (0..n)
        .map(|i| {
            (
                "opportunity.create".to_owned(),
                json!({"company": format!("{label} deal {i}"), "value_cents": value_cents}),
            )
        })
        .collect();
    handle.send_batch(Uuid::new_v4(), commands).await?;
    Ok(())
}

async fn count_rows(path: &Path, like: Option<&str>) -> anyhow::Result<i64> {
    let pool = lula_dog_backend::db::connect(&sqlite_url(path)).await?;
    let n: i64 = match like {
        Some(pattern) => {
            sqlx::query_scalar("SELECT COUNT(*) FROM opportunities WHERE company LIKE ?1")
                .bind(pattern)
                .fetch_one(&pool)
                .await?
        }
        None => {
            sqlx::query_scalar("SELECT COUNT(*) FROM opportunities")
                .fetch_one(&pool)
                .await?
        }
    };
    pool.close().await;
    Ok(n)
}

/// Run one DuckDB script; returns (success, stdout, stderr, wall µs).
async fn duckdb(script: &str) -> anyhow::Result<(bool, String, String, u64)> {
    let start = Instant::now();
    let out = Command::new(DUCKDB_BIN)
        .args(["-no-agent", "-json", "-c", script])
        .output()
        .await?;
    let us = elapsed_us(start);
    Ok((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        us,
    ))
}

fn fanout_script(attach: &[(String, String)]) -> String {
    let mut script = String::from("LOAD sqlite_scanner;\n");
    for (_, stmt) in attach {
        script.push_str(stmt);
        script.push('\n');
    }
    let union: Vec<String> = attach
        .iter()
        .map(|(alias, _)| {
            format!(
                "SELECT '{alias}' AS part, COUNT(*) AS n, CAST(COALESCE(SUM(value_cents), 0) AS BIGINT) AS total \
                 FROM {alias}.opportunities"
            )
        })
        .collect();
    script.push_str(&union.join("\nUNION ALL\n"));
    script.push_str("\nORDER BY part;");
    script
}

/// Correctness scenario: shared + u1 + u2 in workspace A, u1 in workspace B.
#[allow(clippy::too_many_lines)] // one linear scenario script
async fn correctness(data_dir: &Path) -> anyhow::Result<Value> {
    let (ws_a, ws_b) = (Uuid::new_v4(), Uuid::new_v4());
    let shared_a = PartitionKey::shared(ws_a);
    let u1_a = PartitionKey::keyed(ws_a, "user", "u1")?;
    let u2_a = PartitionKey::keyed(ws_a, "user", "u2")?;
    let u1_b = PartitionKey::keyed(ws_b, "user", "u1")?;

    let h_shared = spawn_partition(data_dir, &shared_a).await?;
    let h_u1 = spawn_partition(data_dir, &u1_a).await?;
    let h_u2 = spawn_partition(data_dir, &u2_a).await?;
    let h_b = spawn_partition(data_dir, &u1_b).await?;
    create_deals(&h_shared, "A-shared", 2, 100).await?;
    create_deals(&h_u1, "A-u1", 5, 1_000).await?;
    create_deals(&h_u2, "A-u2", 7, 10_000).await?;
    create_deals(&h_b, "B-u1", 3, 100_000).await?;

    // Atomic within one partition: the third command fails, so the batch
    // applies nothing in u1.
    let bad_batch = vec![
        (
            "opportunity.create".to_owned(),
            json!({"company": "A-u1 extra 1"}),
        ),
        (
            "opportunity.create".to_owned(),
            json!({"company": "A-u1 extra 2"}),
        ),
        (
            "opportunity.update".to_owned(),
            json!({"opportunity_id": 999_999, "next_step": "nope"}),
        ),
    ];
    let bad_rejected = h_u1.send_batch(Uuid::new_v4(), bad_batch).await.is_err();
    let u1_total = count_rows(&u1_a.path(data_dir), None).await?;
    let atomic_ok = bad_rejected && u1_total == 5;

    // Isolation: each file holds only its own rows.
    let isolation_ok = count_rows(&u1_a.path(data_dir), Some("A-u1%")).await? == 5
        && count_rows(&u2_a.path(data_dir), None).await? == 7
        && count_rows(&u2_a.path(data_dir), Some("A-u2%")).await? == 7
        && count_rows(&shared_a.path(data_dir), None).await? == 2
        && count_rows(&u1_b.path(data_dir), None).await? == 3
        && u1_a.path(data_dir) != u1_b.path(data_dir);

    // Hard invariant: the fan-out builder refuses another workspace's file.
    let cross_ws_refused = matches!(
        fanout_attach_sql(
            data_dir,
            ws_a,
            &[shared_a.clone(), u1_b.clone()],
            DEFAULT_FANOUT_LIMIT
        ),
        Err(PartitionError::CrossWorkspace)
    );
    let traversal_refused = PartitionKey::keyed(ws_a, "user", "../../other").is_err();

    // Fan-out read across A's partitions (actors still live; WAL readers).
    let parts = [shared_a.clone(), u1_a.clone(), u2_a.clone()];
    let attach = fanout_attach_sql(data_dir, ws_a, &parts, DEFAULT_FANOUT_LIMIT)?;
    let (ok, stdout, stderr, us) = duckdb(&fanout_script(&attach)).await?;
    let rows: Value = serde_json::from_str(stdout.trim()).unwrap_or(Value::Null);
    let expected = json!([
        {"part": "p0", "n": 2, "total": 200},
        {"part": "p1", "n": 5, "total": 5_000},
        {"part": "p2", "n": 7, "total": 70_000},
    ]);
    let fanout_ok = ok && rows == expected;

    // The fan-out connection can't write.
    let mut write_script = String::from("LOAD sqlite_scanner;\n");
    let (_, u1_attach) = attach
        .get(1)
        .ok_or_else(|| anyhow::anyhow!("missing u1 attach"))?;
    write_script.push_str(u1_attach);
    write_script.push_str("\nINSERT INTO p1.opportunities (company) VALUES ('sneaky');");
    let (write_ok, _, write_err, _) = duckdb(&write_script).await?;
    let read_only_ok = !write_ok && count_rows(&u1_a.path(data_dir), None).await? == 5;

    let passed = atomic_ok
        && isolation_ok
        && cross_ws_refused
        && traversal_refused
        && fanout_ok
        && read_only_ok;
    println!(
        "  correctness: atomic={atomic_ok} isolation={isolation_ok} cross_ws_refused={cross_ws_refused} \
         traversal_refused={traversal_refused} fanout={fanout_ok} ({us}us) read_only={read_only_ok} => {}",
        if passed { "PASS" } else { "FAIL" }
    );
    if !fanout_ok {
        println!("  fanout stdout: {stdout}\n  fanout stderr: {stderr}");
    }
    Ok(json!({
        "passed": passed,
        "atomic_within_partition": atomic_ok,
        "isolation": isolation_ok,
        "cross_workspace_refused": cross_ws_refused,
        "path_traversal_refused": traversal_refused,
        "fanout_rows_correct": fanout_ok,
        "fanout_us": us,
        "fanout_rows": rows,
        "fanout_write_refused": read_only_ok,
        "fanout_write_error": write_err.lines().next().unwrap_or_default(),
    }))
}

/// Scale: shared + N user partitions with `deals` rows each; times waking a
/// new partition and the DuckDB fan-out over all of them.
async fn scale(data_dir: &Path, n_users: usize, deals: usize) -> anyhow::Result<Value> {
    let ws = Uuid::new_v4();
    let mut keys = vec![PartitionKey::shared(ws)];
    let mut handles = Vec::new();
    let mut wake_us = Vec::new();
    for u in 0..n_users {
        keys.push(PartitionKey::keyed(ws, "user", &format!("user-{u}"))?);
    }
    for key in &keys {
        let start = Instant::now();
        let handle = spawn_partition(data_dir, key).await?;
        wake_us.push(elapsed_us(start));
        create_deals(&handle, "scale", deals, 1).await?;
        handles.push(handle);
    }
    let attach = fanout_attach_sql(data_dir, ws, &keys, DEFAULT_FANOUT_LIMIT)?;
    let script = fanout_script(&attach);
    let mut query_us = Vec::new();
    let mut correct = true;
    for _ in 0..QUERY_REPS {
        let (ok, stdout, _, us) = duckdb(&script).await?;
        let rows: Vec<Value> = serde_json::from_str(stdout.trim()).unwrap_or_default();
        let total: i64 = rows.iter().filter_map(|r| r["n"].as_i64()).sum();
        #[allow(clippy::cast_possible_wrap)]
        let want = (keys.len() * deals) as i64;
        correct &= ok && rows.len() == keys.len() && total == want;
        query_us.push(us);
    }
    let mut bytes = 0u64;
    for key in &keys {
        let path = key.path(data_dir);
        bytes += tokio::fs::metadata(&path).await?.len();
        let wal = path.with_extension("db-wal");
        if let Ok(meta) = tokio::fs::metadata(&wal).await {
            bytes += meta.len();
        }
    }
    let wake = stats_from(wake_us, 0);
    let query = stats_from(query_us, 0);
    println!(
        "  scale users={n_users:>3} (+shared) deals/partition={deals} wake p50={:>6}us p95={:>6}us | \
         duckdb fan-out p50={:>8}us p95={:>8}us correct={correct} | files={} bytes={bytes}",
        wake.p50_us,
        wake.p95_us,
        query.p50_us,
        query.p95_us,
        keys.len()
    );
    drop(handles);
    Ok(json!({
        "user_partitions": n_users,
        "files": keys.len(),
        "deals_per_partition": deals,
        "total_bytes_db_plus_wal": bytes,
        "wake": wake,
        "fanout_query": query,
        "fanout_correct": correct,
    }))
}

pub async fn run() -> anyhow::Result<Value> {
    println!("== partitions ==");
    if !Path::new(DUCKDB_BIN).is_file() {
        println!("  duckdb binary not found at {DUCKDB_BIN}; skipping");
        return Ok(json!({ "skipped": true, "reason": "duckdb binary not found" }));
    }
    let dir = tempfile::tempdir()?;
    let correctness = correctness(dir.path()).await?;

    // Process-start + LOAD baseline, so fan-out numbers can be read net of it.
    let mut base_us = Vec::new();
    for _ in 0..QUERY_REPS {
        base_us.push(duckdb("LOAD sqlite_scanner; SELECT 42 AS x;").await?.3);
    }
    let baseline = stats_from(base_us, 0);
    println!(
        "  duckdb process start + LOAD sqlite_scanner baseline p50={}us p95={}us",
        baseline.p50_us, baseline.p95_us
    );

    let mut scales = Vec::new();
    for n in [2, 50, 200] {
        scales.push(scale(dir.path(), n, 200).await?);
    }
    Ok(json!({
        "correctness": correctness,
        "duckdb_baseline": baseline,
        "scale": scales,
    }))
}
