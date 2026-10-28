//! The workspace actor (`src/actor/mod.rs`): commands applied through
//! `dispatch_in`, idempotent replay, batch atomicity, outbox sequencing and
//! wake-after-idle-eviction.

#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a failing assertion aborting the test is the mechanism"
)]

use std::time::Duration;

use lula_dog_backend::actor::{ActorConfig, ActorRegistry, WorkspaceActor, WorkspaceHandle};
use lula_dog_backend::companies::{self, NewCompany};
use lula_dog_backend::error::AppError;
use lula_dog_backend::leads;
use lula_dog_backend::opportunities::{self, NewOpportunity};
use lula_dog_backend::target_profiles::{self, NewTargetProfile};
use serde_json::json;
use sqlx::SqlitePool;
use tempfile::TempDir;
use uuid::Uuid;

/// A fresh opportunity and an open lead candidate (with its company and
/// target profile), for the actions that need one.
async fn seed_fixtures(pool: &SqlitePool) -> (i64, i64) {
    let mut conn = pool.acquire().await.unwrap();
    let opportunity = opportunities::insert(
        &mut conn,
        NewOpportunity {
            company: "Northwind Coffee Roasters".to_owned(),
            contact_name: None,
            contact_email: None,
            value_cents: None,
            stage: None,
            next_step: None,
            last_touch_at: None,
            product: None,
        },
    )
    .await
    .unwrap();
    let profile = target_profiles::insert(
        &mut conn,
        NewTargetProfile {
            name: "Default".to_owned(),
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
    .await
    .unwrap();
    let company = companies::upsert(
        &mut conn,
        NewCompany {
            name: "Beta Boxes LLC".to_owned(),
            jurisdiction: "IL".to_owned(),
            ..NewCompany::default()
        },
    )
    .await
    .unwrap();
    let lead = leads::queue_candidate(&mut conn, company.id, profile.id, 80, Some("good fit"))
        .await
        .unwrap()
        .unwrap();
    (opportunity.id, lead.id)
}

struct Fixture {
    _dir: TempDir,
    path: std::path::PathBuf,
    handle: WorkspaceHandle,
    opportunity_id: i64,
    lead_id: i64,
}

async fn setup() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.db");
    let url = format!("sqlite://{}", path.display());
    let pool = lula_dog_backend::db::connect(&url).await.unwrap();
    let (opportunity_id, lead_id) = seed_fixtures(&pool).await;
    pool.close().await;

    let handle = WorkspaceActor::spawn(&path, ActorConfig::default())
        .await
        .unwrap();
    Fixture {
        _dir: dir,
        path,
        handle,
        opportunity_id,
        lead_id,
    }
}

async fn reopen(path: &std::path::Path) -> SqlitePool {
    lula_dog_backend::db::connect(&format!("sqlite://{}", path.display()))
        .await
        .unwrap()
}

#[tokio::test]
async fn create_update_stage_move_task_create_and_lead_accept_through_the_actor() {
    let f = setup().await;

    let created = f
        .handle
        .send(
            Uuid::new_v4(),
            "opportunity.create",
            json!({"company": "Ridgeline Hardware Supply"}),
        )
        .await
        .unwrap();
    assert!(!created.deduped);
    assert!(created.outbox_seq >= 0);
    let new_opp_id = created.result["id"].as_i64().unwrap();

    let updated = f
        .handle
        .send(
            Uuid::new_v4(),
            "opportunity.update",
            json!({"opportunity_id": new_opp_id, "next_step": "Confirm pallet spec"}),
        )
        .await
        .unwrap();
    assert_eq!(updated.result["next_step"], "Confirm pallet spec");
    assert!(updated.undo.is_some());

    let moved = f
        .handle
        .send(
            Uuid::new_v4(),
            "opportunity.update_stage",
            json!({"opportunity_id": new_opp_id, "stage": "qualified"}),
        )
        .await
        .unwrap();
    assert_eq!(moved.result["stage"], "qualified");

    let task = f
        .handle
        .send(
            Uuid::new_v4(),
            "task.create",
            json!({"opportunity_id": f.opportunity_id, "title": "Ship printed samples"}),
        )
        .await
        .unwrap();
    assert_eq!(task.result["title"], "Ship printed samples");
    assert!(task.undo.is_some());

    let accepted = f
        .handle
        .send(Uuid::new_v4(), "lead.accept", json!({"lead_id": f.lead_id}))
        .await
        .unwrap();
    assert!(accepted.result["opportunity_id"].as_i64().is_some());
}

#[tokio::test]
async fn replaying_the_same_command_id_is_deduped_and_not_applied_twice() {
    let f = setup().await;
    let id = Uuid::new_v4();
    let args = json!({"opportunity_id": f.opportunity_id, "title": "Follow up on samples"});

    let first = f
        .handle
        .send(id, "task.create", args.clone())
        .await
        .unwrap();
    assert!(!first.deduped);
    assert!(first.outbox_seq >= 0);

    let second = f.handle.send(id, "task.create", args).await.unwrap();
    assert!(second.deduped);
    assert_eq!(second.outbox_seq, -1);
    assert_eq!(first.result, second.result);

    let pool = reopen(&f.path).await;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks WHERE opportunity_id = ?1")
        .bind(f.opportunity_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "the task was only inserted once");
}

#[tokio::test]
async fn replaying_the_same_id_with_different_args_is_a_conflict() {
    let f = setup().await;
    let id = Uuid::new_v4();

    let first = f
        .handle
        .send(
            id,
            "task.create",
            json!({"opportunity_id": f.opportunity_id, "title": "Original title"}),
        )
        .await
        .unwrap();
    assert!(!first.deduped);

    let err = f
        .handle
        .send(
            id,
            "task.create",
            json!({"opportunity_id": f.opportunity_id, "title": "Different title"}),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Conflict(_)), "got {err:?}");

    let pool = reopen(&f.path).await;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        count, 1,
        "the conflicting replay must not insert a second task"
    );
}

#[tokio::test]
async fn a_batch_with_a_bad_command_at_the_end_leaves_no_rows() {
    let f = setup().await;
    let commands = vec![
        (
            "task.create".to_owned(),
            json!({"opportunity_id": f.opportunity_id, "title": "Good one"}),
        ),
        (
            "task.create".to_owned(),
            json!({"opportunity_id": f.opportunity_id, "title": "Good two"}),
        ),
        (
            "task.create".to_owned(),
            json!({"opportunity_id": 999_999_i64, "title": "Bad: no such opportunity"}),
        ),
    ];

    let err = f
        .handle
        .send_batch(Uuid::new_v4(), commands)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)), "got {err:?}");

    let pool = reopen(&f.path).await;
    let task_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(task_count, 0, "no task from the batch should have landed");
    let command_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _lula_commands")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(command_count, 0, "the failed batch must not be recorded");
    let outbox_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _lula_outbox")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        outbox_count, 0,
        "the failed batch must not reach the outbox"
    );
}

#[tokio::test]
async fn a_full_batch_applies_in_one_transaction_and_is_idempotent() {
    let f = setup().await;
    let id = Uuid::new_v4();
    let commands = vec![
        (
            "task.create".to_owned(),
            json!({"opportunity_id": f.opportunity_id, "title": "Batch task one"}),
        ),
        (
            "task.create".to_owned(),
            json!({"opportunity_id": f.opportunity_id, "title": "Batch task two"}),
        ),
    ];

    let first = f.handle.send_batch(id, commands.clone()).await.unwrap();
    assert!(!first.deduped);
    assert!(first.outbox_seq >= 0);

    let second = f.handle.send_batch(id, commands).await.unwrap();
    assert!(second.deduped);
    assert_eq!(second.outbox_seq, -1);

    let pool = reopen(&f.path).await;
    let task_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(task_count, 2, "the batch's two tasks landed exactly once");
}

#[tokio::test]
async fn outbox_seq_increases_with_each_applied_command() {
    let f = setup().await;
    let first = f
        .handle
        .send(
            Uuid::new_v4(),
            "task.create",
            json!({"opportunity_id": f.opportunity_id, "title": "One"}),
        )
        .await
        .unwrap();
    let second = f
        .handle
        .send(
            Uuid::new_v4(),
            "task.create",
            json!({"opportunity_id": f.opportunity_id, "title": "Two"}),
        )
        .await
        .unwrap();
    assert!(first.outbox_seq >= 1);
    assert!(
        second.outbox_seq > first.outbox_seq,
        "seq must strictly increase: {} then {}",
        first.outbox_seq,
        second.outbox_seq
    );
}

#[tokio::test]
async fn wake_after_idle_eviction_reopens_the_same_file() {
    let dir = tempfile::tempdir().unwrap();
    let id = Uuid::new_v4();
    let config = ActorConfig {
        idle_timeout: Duration::from_millis(50),
        ..ActorConfig::default()
    };
    let registry = ActorRegistry::new(dir.path(), config);

    let path = registry.path_for(id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let pool = reopen(&path).await;
    let (opportunity_id, _lead_id) = seed_fixtures(&pool).await;
    pool.close().await;

    let handle = registry.get_or_wake(id).await.unwrap();
    let before = handle
        .send(
            Uuid::new_v4(),
            "task.create",
            json!({"opportunity_id": opportunity_id, "title": "Before sleep"}),
        )
        .await
        .unwrap();
    assert!(!before.deduped);

    // Outlast the idle timeout with no further messages so the actor
    // checkpoints, closes its connection and exits.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(handle.is_closed(), "the actor should have evicted itself");

    let woken = registry.get_or_wake(id).await.unwrap();
    assert!(!woken.is_closed());
    let after = woken
        .send(
            Uuid::new_v4(),
            "task.create",
            json!({"opportunity_id": opportunity_id, "title": "After wake"}),
        )
        .await
        .unwrap();
    assert!(!after.deduped);

    let check = reopen(&path).await;
    let titles: Vec<String> = sqlx::query_scalar("SELECT title FROM tasks ORDER BY id")
        .fetch_all(&check)
        .await
        .unwrap();
    assert_eq!(
        titles,
        vec!["Before sleep".to_owned(), "After wake".to_owned()],
        "data written before the idle sleep must still be there after the wake"
    );
}

#[tokio::test]
async fn concurrent_wakes_of_a_sleeping_workspace_spawn_exactly_one_actor() {
    let dir = tempfile::tempdir().unwrap();
    let id = Uuid::new_v4();
    let registry = ActorRegistry::new(dir.path(), ActorConfig::default());

    let path = registry.path_for(id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let pool = reopen(&path).await;
    let (_opportunity_id, _lead_id) = seed_fixtures(&pool).await;
    pool.close().await;

    // `tokio::join!` polls all eight calls cooperatively on this one test
    // task, rather than spawning them onto worker threads: that keeps the
    // race entirely inside `get_or_wake`'s own per-workspace slot lock
    // (which is what's actually being tested) instead of also exercising
    // `tokio::spawn`'s unrelated `Send + 'static` bound.
    let (r1, r2, r3, r4, r5, r6, r7, r8) = tokio::join!(
        registry.get_or_wake(id),
        registry.get_or_wake(id),
        registry.get_or_wake(id),
        registry.get_or_wake(id),
        registry.get_or_wake(id),
        registry.get_or_wake(id),
        registry.get_or_wake(id),
        registry.get_or_wake(id),
    );
    let handles: Vec<WorkspaceHandle> = vec![r1, r2, r3, r4, r5, r6, r7, r8]
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    let first = &handles[0];
    for other in &handles {
        assert!(
            first.same_actor(other),
            "every concurrent waker must land on the same actor, not a fresh spawn each"
        );
    }
}
