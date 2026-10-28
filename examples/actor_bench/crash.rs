//! `crash` mode: spawn `actor_bench` as a child process, `kill -9` it
//! mid-transaction (or mid-loop), reopen the database and check that the
//! in-flight write left no trace, that a retry applies it exactly once,
//! and that a second retry is a no-op dedupe hit.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use lula_dog_backend::actor::{ActorConfig, WorkspaceActor};
use rand::Rng;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use uuid::Uuid;

use crate::common::{fresh_db, loop_batch_commands, sqlite_url};

const REPS: usize = 20;
const IN_TX_TIMEOUT: Duration = Duration::from_secs(5);
const CRASH_DELAY_MS: u64 = 3_000;

fn bench_exe() -> anyhow::Result<std::path::PathBuf> {
    std::env::current_exe().map_err(|e| anyhow::anyhow!("current_exe: {e}"))
}

fn spawn_child(
    scenario: &str,
    db: &Path,
    extra_args: &[String],
    crash_delay_ms: Option<u64>,
) -> anyhow::Result<Child> {
    let exe = bench_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg("crash-child").arg(scenario).arg(db.as_os_str());
    for a in extra_args {
        cmd.arg(a);
    }
    if let Some(ms) = crash_delay_ms {
        cmd.env("LULA_ACTOR_CRASH_DELAY_MS", ms.to_string());
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.spawn()
        .map_err(|e| anyhow::anyhow!("spawning crash-child: {e}"))
}

/// Reads the child's stdout until it prints `needle` or `timeout` elapses.
async fn wait_for_line(child: &mut Child, needle: &str, timeout: Duration) -> bool {
    let Some(stdout) = child.stdout.take() else {
        return false;
    };
    let mut lines = BufReader::new(stdout).lines();
    let search = async {
        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim() == needle {
                return true;
            }
        }
        false
    };
    tokio::time::timeout(timeout, search).await.unwrap_or(false)
}

async fn kill_and_reap(child: &mut Child) {
    let _ = child.kill().await;
    let _ = child.wait().await;
}

async fn integrity_ok(path: &Path) -> anyhow::Result<bool> {
    let pool = lula_dog_backend::db::connect(&sqlite_url(path)).await?;
    let result: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&pool)
        .await?;
    pool.close().await;
    Ok(result == "ok")
}

struct ScenarioResult {
    passed: bool,
    recovery_us: u64,
}

/// Scenario (i): a single `lead.accept`, killed mid-commit.
async fn scenario_single() -> anyhow::Result<ScenarioResult> {
    let db = fresh_db(1, 5).await?;
    let lead_id = db
        .seed
        .lead_ids
        .first()
        .copied()
        .ok_or_else(|| anyhow::anyhow!("no seeded lead"))?;
    let cmd_id = Uuid::new_v4();

    let mut child = spawn_child(
        "single",
        &db.path,
        &[cmd_id.to_string(), lead_id.to_string()],
        Some(CRASH_DELAY_MS),
    )?;
    let saw_in_tx = wait_for_line(&mut child, "IN_TX", IN_TX_TIMEOUT).await;
    kill_and_reap(&mut child).await;
    if !saw_in_tx {
        return Ok(ScenarioResult {
            passed: false,
            recovery_us: 0,
        });
    }

    if !integrity_ok(&db.path).await? {
        return Ok(ScenarioResult {
            passed: false,
            recovery_us: 0,
        });
    }
    let pool = lula_dog_backend::db::connect(&sqlite_url(&db.path)).await?;
    let recorded: Option<String> =
        sqlx::query_scalar("SELECT id FROM _lula_commands WHERE id = ?1")
            .bind(cmd_id.to_string())
            .fetch_optional(&pool)
            .await?;
    let lead_status: String =
        sqlx::query_scalar("SELECT status FROM lead_candidates WHERE id = ?1")
            .bind(lead_id)
            .fetch_one(&pool)
            .await?;
    pool.close().await;
    if recorded.is_some() || lead_status != "new" {
        return Ok(ScenarioResult {
            passed: false,
            recovery_us: 0,
        });
    }

    let recovery_start = Instant::now();
    let handle = WorkspaceActor::spawn(&db.path, ActorConfig::default()).await?;
    let recovery_us = crate::common::elapsed_us(recovery_start);

    let first = handle
        .send(cmd_id, "lead.accept", json!({"lead_id": lead_id}))
        .await?;
    let second = handle
        .send(cmd_id, "lead.accept", json!({"lead_id": lead_id}))
        .await?;

    let pool2 = lula_dog_backend::db::connect(&sqlite_url(&db.path)).await?;
    let opportunity_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM opportunities")
        .fetch_one(&pool2)
        .await?;
    pool2.close().await;

    let passed = !first.deduped && second.deduped && opportunity_count == 2 /* 1 seeded + 1 from accept */;
    Ok(ScenarioResult {
        passed,
        recovery_us,
    })
}

/// Scenario (ii): a 500-command batch, killed mid-commit.
async fn scenario_mid_batch() -> anyhow::Result<ScenarioResult> {
    let db = fresh_db(1, 0).await?;
    let opportunity_id = db
        .seed
        .opportunity_ids
        .first()
        .copied()
        .ok_or_else(|| anyhow::anyhow!("no seeded opportunity"))?;
    let batch_id = Uuid::new_v4();

    let mut child = spawn_child(
        "mid_batch",
        &db.path,
        &[batch_id.to_string(), opportunity_id.to_string()],
        Some(CRASH_DELAY_MS),
    )?;
    let saw_in_tx = wait_for_line(&mut child, "IN_TX", IN_TX_TIMEOUT).await;
    kill_and_reap(&mut child).await;
    if !saw_in_tx {
        return Ok(ScenarioResult {
            passed: false,
            recovery_us: 0,
        });
    }

    if !integrity_ok(&db.path).await? {
        return Ok(ScenarioResult {
            passed: false,
            recovery_us: 0,
        });
    }
    let pool = lula_dog_backend::db::connect(&sqlite_url(&db.path)).await?;
    let recorded: Option<String> =
        sqlx::query_scalar("SELECT id FROM _lula_commands WHERE id = ?1")
            .bind(batch_id.to_string())
            .fetch_optional(&pool)
            .await?;
    let task_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks")
        .fetch_one(&pool)
        .await?;
    pool.close().await;
    if recorded.is_some() || task_count != 0 {
        return Ok(ScenarioResult {
            passed: false,
            recovery_us: 0,
        });
    }

    let recovery_start = Instant::now();
    let handle = WorkspaceActor::spawn(&db.path, ActorConfig::default()).await?;
    let recovery_us = crate::common::elapsed_us(recovery_start);

    let commands = loop_batch_commands(opportunity_id, 0, 500);
    let first = handle.send_batch(batch_id, commands.clone()).await?;
    let second = handle.send_batch(batch_id, commands).await?;

    let pool2 = lula_dog_backend::db::connect(&sqlite_url(&db.path)).await?;
    let task_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks")
        .fetch_one(&pool2)
        .await?;
    pool2.close().await;

    let passed = !first.deduped && second.deduped && task_count_after == 500;
    Ok(ScenarioResult {
        passed,
        recovery_us,
    })
}

const LOOP_BATCHES: usize = 10;
const LOOP_BATCH_SIZE: usize = 20;

/// Scenario (iii): `LOOP_BATCHES` batches committed one after another at
/// normal speed, killed at a random point between commits.
#[allow(clippy::too_many_lines)] // one linear scenario script; splitting it hurts readability
async fn scenario_between_batches() -> anyhow::Result<ScenarioResult> {
    let db = fresh_db(1, 0).await?;
    let opportunity_id = db
        .seed
        .opportunity_ids
        .first()
        .copied()
        .ok_or_else(|| anyhow::anyhow!("no seeded opportunity"))?;
    let ids: Vec<Uuid> = (0..LOOP_BATCHES).map(|_| Uuid::new_v4()).collect();
    let id_args: Vec<String> = ids.iter().map(Uuid::to_string).collect();

    let mut child = spawn_child(
        "between_batches",
        &db.path,
        &std::iter::once(opportunity_id.to_string())
            .chain(id_args)
            .collect::<Vec<_>>(),
        None,
    )?;
    let delay_ms = rand::thread_rng().gen_range(2..=30);
    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
    kill_and_reap(&mut child).await;

    if !integrity_ok(&db.path).await? {
        return Ok(ScenarioResult {
            passed: false,
            recovery_us: 0,
        });
    }
    let pool = lula_dog_backend::db::connect(&sqlite_url(&db.path)).await?;
    let committed_batches: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _lula_commands WHERE action = 'batch'")
            .fetch_one(&pool)
            .await?;
    let task_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks")
        .fetch_one(&pool)
        .await?;
    #[allow(
        clippy::cast_possible_wrap,
        reason = "LOOP_BATCH_SIZE is a tiny compile-time constant, never near i64::MAX"
    )]
    let expected_task_count = committed_batches * (LOOP_BATCH_SIZE as i64);
    let no_partial_batch = task_count == expected_task_count;

    // The ids the loop never reached must be absent; the ones it did
    // commit must be present, and in order (the child commits
    // sequentially).
    let mut ids_consistent = true;
    let committed_usize = usize::try_from(committed_batches).unwrap_or(0);
    for (index, id) in ids.iter().enumerate() {
        let present: Option<String> =
            sqlx::query_scalar("SELECT id FROM _lula_commands WHERE id = ?1")
                .bind(id.to_string())
                .fetch_optional(&pool)
                .await?;
        let should_be_present = index < committed_usize;
        if present.is_some() != should_be_present {
            ids_consistent = false;
        }
    }
    pool.close().await;

    if !no_partial_batch || !ids_consistent {
        return Ok(ScenarioResult {
            passed: false,
            recovery_us: 0,
        });
    }

    let recovery_start = Instant::now();
    let handle = WorkspaceActor::spawn(&db.path, ActorConfig::default()).await?;
    let recovery_us = crate::common::elapsed_us(recovery_start);

    // Retry every id: already-committed ones dedupe, the rest apply fresh.
    for (index, id) in ids.iter().enumerate() {
        let commands = loop_batch_commands(opportunity_id, index, LOOP_BATCH_SIZE);
        handle.send_batch(*id, commands).await?;
    }
    let pool2 = lula_dog_backend::db::connect(&sqlite_url(&db.path)).await?;
    let task_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks")
        .fetch_one(&pool2)
        .await?;
    #[allow(
        clippy::cast_possible_wrap,
        reason = "LOOP_BATCHES * LOOP_BATCH_SIZE is tiny"
    )]
    let expected_after = (LOOP_BATCHES * LOOP_BATCH_SIZE) as i64;
    pool2.close().await;
    if task_count_after != expected_after {
        return Ok(ScenarioResult {
            passed: false,
            recovery_us,
        });
    }

    // Second full retry: everything should dedupe, no new rows.
    for (index, id) in ids.iter().enumerate() {
        let commands = loop_batch_commands(opportunity_id, index, LOOP_BATCH_SIZE);
        let reply = handle.send_batch(*id, commands).await?;
        if !reply.deduped {
            return Ok(ScenarioResult {
                passed: false,
                recovery_us,
            });
        }
    }
    let pool3 = lula_dog_backend::db::connect(&sqlite_url(&db.path)).await?;
    let task_count_final: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks")
        .fetch_one(&pool3)
        .await?;
    pool3.close().await;

    Ok(ScenarioResult {
        passed: task_count_final == expected_after,
        recovery_us,
    })
}

async fn run_scenario<F, Fut>(name: &str, reps: usize, scenario: F) -> anyhow::Result<Value>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<ScenarioResult>>,
{
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut recoveries = Vec::with_capacity(reps);
    for _ in 0..reps {
        match scenario().await {
            Ok(result) => {
                if result.passed {
                    passed += 1;
                } else {
                    failed += 1;
                }
                if result.recovery_us > 0 {
                    recoveries.push(result.recovery_us);
                }
            }
            Err(_) => failed += 1,
        }
    }
    let recovery_stats = crate::common::stats_from(recoveries, 0);
    println!(
        "  {name:<16} pass={passed:>2}/{reps} fail={failed:>2}/{reps} \
         recovery p50={:>6}us p95={:>6}us",
        recovery_stats.p50_us, recovery_stats.p95_us,
    );
    Ok(json!({
        "scenario": name,
        "reps": reps,
        "passed": passed,
        "failed": failed,
        "recovery_us": recovery_stats,
    }))
}

pub async fn run() -> anyhow::Result<Value> {
    println!("== crash ==");
    let single = run_scenario("single", REPS, scenario_single).await?;
    let mid_batch = run_scenario("mid_batch", REPS, scenario_mid_batch).await?;
    let between_batches = run_scenario("between_batches", REPS, scenario_between_batches).await?;
    Ok(json!({
        "single": single,
        "mid_batch": mid_batch,
        "between_batches": between_batches,
    }))
}
