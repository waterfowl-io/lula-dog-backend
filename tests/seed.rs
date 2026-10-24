//! The seed command produces the data the README promises.

#![expect(
    clippy::unwrap_used,
    reason = "a failing assertion aborting the test is the mechanism"
)]

mod common;

use common::TestApp;
use lula_dog_backend::seed;
use serde_json::Value;

#[tokio::test]
async fn seeds_an_empty_database_once() {
    let app = TestApp::new().await;
    let summary = seed::run(&app.pool, 50).await.unwrap();
    assert_eq!(summary.opportunities, 8);
    assert_eq!(summary.leads, 4);

    let leads: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM lead_candidates")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(leads, 4);
    let companies: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM companies")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(companies, 4);

    let (_, all) = app.get("/opportunities").await;
    let all = all.as_array().unwrap();
    assert_eq!(all.len(), 8);
    let stages: Vec<&str> = all.iter().map(|o| o["stage"].as_str().unwrap()).collect();
    for stage in ["lead", "qualified", "proposal", "won", "lost"] {
        assert!(stages.contains(&stage), "no {stage} deal");
    }

    let (_, stale) = app.get("/alerts/stale").await;
    let stale: Vec<&str> = stale
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["company"].as_str().unwrap())
        .collect();
    assert_eq!(stale, vec!["Bluebird Apparel", "Ridgeline Hardware Supply"]);

    let pending: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM field_changes WHERE status = 'proposed'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert!(pending >= 1, "seed should leave a pending proposal");
    let entries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM timeline_entries")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert!(entries >= 3);
    let open_tasks: i64 = all
        .iter()
        .map(|o: &Value| o["open_task_count"].as_i64().unwrap())
        .sum();
    assert!(open_tasks >= 3);

    // Re-running against an already-seeded database is the normal upgrade
    // path (a developer's persistent DATA_DIR) -- it must not error, and
    // must not duplicate the deals or leads that are already there.
    let summary = seed::run(&app.pool, 50).await.unwrap();
    assert_eq!(
        summary.opportunities, 0,
        "existing opportunities must not be re-seeded"
    );
    assert_eq!(summary.leads, 0, "existing leads must not be re-seeded");

    let opportunities: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM opportunities")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        opportunities, 8,
        "second run must not duplicate opportunities"
    );
    let leads: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM lead_candidates")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(leads, 4, "second run must not duplicate leads");
}

/// A developer's existing dev database, seeded before the example leads
/// existed: opportunities are already there, but `lead_candidates` is
/// empty. Re-running `seed::run` (the `cargo run -- seed` upgrade path)
/// must backfill the leads without touching the existing deals, and a
/// further run must not duplicate them.
#[tokio::test]
async fn backfills_leads_into_an_existing_database_missing_them() {
    let app = TestApp::new().await;

    let mut conn = app.pool.acquire().await.unwrap();
    lula_dog_backend::target_profiles::seed_default(&mut conn, 50)
        .await
        .unwrap();
    lula_dog_backend::opportunities::insert(
        &mut conn,
        lula_dog_backend::opportunities::NewOpportunity {
            company: "Existing Co".to_owned(),
            contact_name: None,
            contact_email: None,
            value_cents: None,
            stage: Some(lula_dog_backend::opportunities::Stage::Lead),
            next_step: None,
            last_touch_at: None,
        },
    )
    .await
    .unwrap();
    drop(conn);

    let summary = seed::run(&app.pool, 50).await.unwrap();
    assert_eq!(
        summary.opportunities, 0,
        "existing opportunities must not be re-seeded"
    );
    assert_eq!(summary.leads, 4, "missing leads must be backfilled");

    let opportunities: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM opportunities")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        opportunities, 1,
        "pre-existing opportunity must be untouched"
    );
    let leads: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM lead_candidates")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(leads, 4);

    // A third run must not duplicate the now-present leads.
    let summary = seed::run(&app.pool, 50).await.unwrap();
    assert_eq!(summary.leads, 0, "third run must not duplicate leads");
    let leads: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM lead_candidates")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(leads, 4, "third run must not duplicate leads");
}

/// Re-running `seed_control_plane` against a workspace that already has the
/// sales app must converge it to the current definition (a new published
/// version) rather than erroring or leaving it stale -- this is the
/// upgrade path a developer relies on when re-seeding an existing dev
/// database. Gated on `TEST_CONTROL_DATABASE_URL`.
#[tokio::test]
async fn seed_control_plane_converges_an_existing_workspace() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let control = app.control.as_ref().unwrap();

    seed::seed_control_plane(control, app.workspace_id)
        .await
        .unwrap();
    let app_def =
        lula_dog_backend::control::app_definitions::find_by_key(control, app.workspace_id, "sales")
            .await
            .unwrap()
            .unwrap();
    let first_published = app_def.published_version_id;
    assert!(first_published.is_some());

    // Re-running with the exact same definition must not create a new
    // version (no-op convergence).
    seed::seed_control_plane(control, app.workspace_id)
        .await
        .unwrap();
    let app_def =
        lula_dog_backend::control::app_definitions::fetch(control, app.workspace_id, app_def.id)
            .await
            .unwrap();
    assert_eq!(app_def.published_version_id, first_published);

    let versions = lula_dog_backend::control::app_definitions::list_versions(control, app_def.id)
        .await
        .unwrap();
    assert_eq!(
        versions.len(),
        1,
        "unchanged definition must not re-version"
    );
}
