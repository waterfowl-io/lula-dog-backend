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
    let summary = seed::run(&app.pool).await.unwrap();
    assert_eq!(summary.opportunities, 8);

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

    assert!(
        seed::run(&app.pool).await.is_err(),
        "second run must refuse"
    );
}
