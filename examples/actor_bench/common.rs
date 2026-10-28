//! Shared seeding, workload generation and latency-stats helpers for every
//! `actor_bench` mode.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use lula_dog_backend::actions;
use lula_dog_backend::actor::WorkspaceHandle;
use lula_dog_backend::companies::{self, NewCompany};
use lula_dog_backend::error::{AppError, AppResult};
use lula_dog_backend::leads;
use lula_dog_backend::opportunities::{self, NewOpportunity};
use lula_dog_backend::target_profiles::{self, NewTargetProfile};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use uuid::Uuid;

/// Opportunity and open-lead ids seeded into a fresh workspace database, for
/// the workload generator to spread load across.
#[derive(Default)]
pub struct SeedIds {
    pub opportunity_ids: Vec<i64>,
    pub lead_ids: Vec<i64>,
}

/// Seed `n_opportunities` opportunities and `n_leads` open lead candidates
/// (with their companies and one shared target profile) into the database
/// at `url`. Mirrors `src/seed.rs`'s shape at benchmark scale rather than
/// its hand-tuned dev fixtures.
pub async fn seed(url: &str, n_opportunities: usize, n_leads: usize) -> anyhow::Result<SeedIds> {
    let pool = lula_dog_backend::db::connect(url).await?;
    let mut conn = pool.acquire().await?;

    let mut opportunity_ids = Vec::with_capacity(n_opportunities);
    for i in 0..n_opportunities {
        let created = opportunities::insert(
            &mut conn,
            NewOpportunity {
                company: format!("Seed Co {i}"),
                contact_name: None,
                contact_email: None,
                value_cents: Some(100_000),
                stage: None,
                next_step: None,
                last_touch_at: None,
                product: None,
            },
        )
        .await?;
        opportunity_ids.push(created.id);
    }

    let profile = target_profiles::insert(
        &mut conn,
        NewTargetProfile {
            name: "Bench profile".to_owned(),
            industries: vec![],
            regions: vec![],
            size_min: None,
            size_max: None,
            size_notes: None,
            include_keywords: vec![],
            exclude_keywords: vec![],
            min_fit_score: None,
        },
    )
    .await?;

    let mut lead_ids = Vec::with_capacity(n_leads);
    for i in 0..n_leads {
        let company = companies::upsert(
            &mut conn,
            NewCompany {
                name: format!("Lead Co {i}"),
                jurisdiction: "IL".to_owned(),
                website: Some(format!("https://lead-{i}.example")),
                ..NewCompany::default()
            },
        )
        .await?;
        if let Some(lead) =
            leads::queue_candidate(&mut conn, company.id, profile.id, 75, Some("bench seed"))
                .await?
        {
            lead_ids.push(lead.id);
        }
    }

    drop(conn);
    pool.close().await;
    Ok(SeedIds {
        opportunity_ids,
        lead_ids,
    })
}

/// The path a bench run's workspace database lives at, under a temp dir.
pub fn db_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.db"))
}

pub fn sqlite_url(path: &Path) -> String {
    format!("sqlite://{}", path.display())
}

/// Where a command (direct dispatch or through the actor) gets applied.
/// Both return the crate's own `AppError`, so one timing loop drives
/// either path.
#[derive(Clone)]
pub enum Target {
    Direct(SqlitePool),
    Actor(WorkspaceHandle),
}

impl Target {
    pub async fn call(&self, action: &str, args: Value) -> AppResult<()> {
        match self {
            Self::Direct(pool) => actions::dispatch(pool, action, &args).await.map(|_| ()),
            Self::Actor(handle) => handle.send(Uuid::new_v4(), action, args).await.map(|_| ()),
        }
    }
}

/// Round-robins through the five workload actions, index 0..5, so
/// `idx % 5` always reproduces the same action for the same `idx`. Only
/// `idx % 5 == 4` (lead.accept) needs a never-repeated id, which `idx / 5`
/// gives for free since it is monotonic in `idx`.
pub fn build_command(idx: usize, seed: &SeedIds) -> (&'static str, Value) {
    match idx % 5 {
        0 => (
            "opportunity.create",
            json!({"company": format!("Bench Co {idx}")}),
        ),
        1 => {
            let id = pick(&seed.opportunity_ids, idx);
            (
                "opportunity.update",
                json!({"opportunity_id": id, "next_step": format!("Follow up {idx}")}),
            )
        }
        2 => {
            let id = pick(&seed.opportunity_ids, idx);
            let stages = ["qualified", "proposal", "won", "lost"];
            let stage = stages
                .get(idx % stages.len())
                .copied()
                .unwrap_or("qualified");
            (
                "opportunity.update_stage",
                json!({"opportunity_id": id, "stage": stage}),
            )
        }
        3 => {
            let id = pick(&seed.opportunity_ids, idx);
            (
                "task.create",
                json!({"opportunity_id": id, "title": format!("Bench task {idx}")}),
            )
        }
        _ => {
            let lead_idx = idx / 5;
            let id = pick(&seed.lead_ids, lead_idx);
            ("lead.accept", json!({"lead_id": id}))
        }
    }
}

fn pick(ids: &[i64], idx: usize) -> i64 {
    if ids.is_empty() {
        return 0;
    }
    ids.get(idx % ids.len()).copied().unwrap_or(0)
}

pub fn elapsed_us(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX)
}

/// `true` for a direct-mode `AppError::Database` whose message indicates
/// SQLite returned `SQLITE_BUSY`/`SQLITE_LOCKED` rather than some other
/// database error.
pub fn is_busy_or_locked(err: &AppError) -> bool {
    if let AppError::Database(e) = err {
        let msg = e.to_string().to_ascii_lowercase();
        msg.contains("locked") || msg.contains("busy")
    } else {
        false
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct LatencyStats {
    pub count: usize,
    pub errors: usize,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub max_us: u64,
}

/// Integer percentile index (`(n - 1) * num / den`), avoiding float casts.
fn percentile(samples_us: &[u64], num: usize, den: usize) -> u64 {
    let n = samples_us.len();
    if n == 0 {
        return 0;
    }
    let idx = (n - 1) * num / den;
    samples_us.get(idx).copied().unwrap_or(0)
}

pub fn stats_from(mut samples_us: Vec<u64>, errors: usize) -> LatencyStats {
    samples_us.sort_unstable();
    LatencyStats {
        count: samples_us.len(),
        errors,
        p50_us: percentile(&samples_us, 50, 100),
        p95_us: percentile(&samples_us, 95, 100),
        p99_us: percentile(&samples_us, 99, 100),
        max_us: samples_us.last().copied().unwrap_or(0),
    }
}

/// Run `warmup + n` commands against `target` spread over `concurrency`
/// workers (1 worker for `concurrency == 1`), discarding the first `warmup`
/// results; `idx` is unique and monotonic across the whole call so
/// `lead.accept`'s distinct-id requirement holds across the warmup/measured
/// boundary too.
pub async fn run_concurrent(
    target: Target,
    seed: Arc<SeedIds>,
    warmup: usize,
    n: usize,
    concurrency: usize,
) -> (Vec<u64>, Vec<AppError>) {
    let total = warmup + n;
    let counter = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::with_capacity(concurrency);
    for _ in 0..concurrency.max(1) {
        let target = target.clone();
        let seed = seed.clone();
        let counter = counter.clone();
        tasks.push(tokio::spawn(async move {
            let mut samples = Vec::new();
            let mut errors = Vec::new();
            loop {
                let idx = counter.fetch_add(1, Ordering::Relaxed);
                if idx >= total {
                    break;
                }
                let (action, args) = build_command(idx, &seed);
                let start = Instant::now();
                let outcome = target.call(action, args).await;
                if idx < warmup {
                    continue;
                }
                match outcome {
                    Ok(()) => samples.push(elapsed_us(start)),
                    Err(e) => errors.push(e),
                }
            }
            (samples, errors)
        }));
    }
    let mut all_samples = Vec::new();
    let mut all_errors = Vec::new();
    for task in tasks {
        if let Ok((samples, errors)) = task.await {
            all_samples.extend(samples);
            all_errors.extend(errors);
        }
    }
    (all_samples, all_errors)
}

/// A fresh temp dir with one seeded workspace database, for one benchmark
/// measurement -- kept alive by the caller for as long as the database is
/// in use.
pub struct FreshDb {
    pub dir: tempfile::TempDir,
    pub path: PathBuf,
    pub seed: Arc<SeedIds>,
}

pub async fn fresh_db(n_opportunities: usize, n_leads: usize) -> anyhow::Result<FreshDb> {
    let dir = tempfile::tempdir()?;
    let path = db_path(dir.path(), "bench");
    let ids = seed(&sqlite_url(&path), n_opportunities, n_leads).await?;
    Ok(FreshDb {
        dir,
        path,
        seed: Arc::new(ids),
    })
}

/// One batch's `task.create` commands for the crash harness's "committed
/// batches in a loop" scenario -- deterministic in `opportunity_id` and
/// `batch_index` so a retry after a crash reconstructs byte-identical args
/// (needed for the idempotency hash to match on replay).
pub fn loop_batch_commands(
    opportunity_id: i64,
    batch_index: usize,
    batch_size: usize,
) -> Vec<(String, Value)> {
    (0..batch_size)
        .map(|i| {
            (
                "task.create".to_owned(),
                json!({
                    "opportunity_id": opportunity_id,
                    "title": format!("Loop batch {batch_index} item {i}"),
                }),
            )
        })
        .collect()
}
