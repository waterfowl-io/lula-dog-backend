//! The `stale` flag and GET /alerts/stale.

#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a failing assertion aborting the test is the mechanism"
)]

mod common;

use axum::http::StatusCode;
use common::{TestApp, days_ago};
use serde_json::{Value, json};

fn companies(list: &Value) -> Vec<&str> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|o| o["company"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn stale_rules() {
    let app = TestApp::new().await;

    let fresh = app
        .create_opportunity(json!({"company": "Fresh", "last_touch_at": days_ago(3)}))
        .await;
    app.create_opportunity(json!({"company": "Old touch", "last_touch_at": days_ago(15)}))
        .await;
    app.create_opportunity(json!({"company": "Older touch", "last_touch_at": days_ago(40)}))
        .await;
    app.create_opportunity(json!({"company": "Just under", "last_touch_at": days_ago(13)}))
        .await;
    // Closed deals are never stale, however old.
    app.create_opportunity(json!({
        "company": "Won long ago", "stage": "won", "last_touch_at": days_ago(90),
    }))
    .await;
    app.create_opportunity(json!({
        "company": "Lost long ago", "stage": "lost", "last_touch_at": days_ago(90),
    }))
    .await;

    // Recently touched, but an open task is overdue.
    let overdue = app
        .create_opportunity(json!({"company": "Overdue task", "last_touch_at": days_ago(1)}))
        .await;
    app.post(
        &format!("/opportunities/{overdue}/tasks"),
        json!({"title": "Send pallet quote", "due_at": days_ago(2)}),
    )
    .await;
    // A done overdue task, or one due in the future, does not count.
    let (_, done) = app
        .post(
            &format!("/opportunities/{fresh}/tasks"),
            json!({"title": "Done already", "due_at": days_ago(5)}),
        )
        .await;
    app.patch(&format!("/tasks/{}", done["id"]), json!({"done": true}))
        .await;
    app.post(
        &format!("/opportunities/{fresh}/tasks"),
        json!({"title": "Due later", "due_at": days_ago(-5)}),
    )
    .await;

    // Never touched: falls back to created_at.
    let untouched_old = app
        .create_opportunity(json!({"company": "Never touched, old"}))
        .await;
    sqlx::query("UPDATE opportunities SET created_at = ?1 WHERE id = ?2")
        .bind(days_ago(20))
        .bind(untouched_old)
        .execute(&app.pool)
        .await
        .unwrap();
    app.create_opportunity(json!({"company": "Never touched, new"}))
        .await;

    let (status, alerts) = app.get("/alerts/stale").await;
    assert_eq!(status, StatusCode::OK);
    // Longest without a touch first.
    assert_eq!(
        companies(&alerts),
        vec![
            "Older touch",
            "Never touched, old",
            "Old touch",
            "Overdue task"
        ]
    );
    for alert in alerts.as_array().unwrap() {
        assert_eq!(alert["stale"], true);
    }
    let overdue_alert = &alerts[3];
    assert_eq!(overdue_alert["overdue_task_count"], 1);
    assert_eq!(overdue_alert["open_task_count"], 1);

    // The list carries the same flag.
    let (_, all) = app.get("/opportunities").await;
    let all = all.as_array().unwrap();
    assert_eq!(all.len(), 9);
    let flag = |name: &str| {
        all.iter()
            .find(|o| o["company"] == name)
            .map(|o| o["stale"].clone())
            .unwrap()
    };
    assert_eq!(flag("Fresh"), json!(false));
    assert_eq!(flag("Just under"), json!(false));
    assert_eq!(flag("Won long ago"), json!(false));
    assert_eq!(flag("Lost long ago"), json!(false));
    assert_eq!(flag("Never touched, new"), json!(false));
    assert_eq!(flag("Old touch"), json!(true));

    let (_, fresh_detail) = app.get(&format!("/opportunities/{fresh}")).await;
    assert_eq!(fresh_detail["opportunity"]["open_task_count"], 1);
    assert_eq!(fresh_detail["opportunity"]["overdue_task_count"], 0);
}

#[tokio::test]
async fn a_task_due_today_is_not_overdue() {
    let app = TestApp::new().await;
    let id = app
        .create_opportunity(json!({"company": "Today", "last_touch_at": days_ago(1)}))
        .await;
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    app.post(
        &format!("/opportunities/{id}/tasks"),
        json!({"title": "Due today", "due_at": today}),
    )
    .await;
    let (_, alerts) = app.get("/alerts/stale").await;
    assert_eq!(alerts, json!([]));
}

#[tokio::test]
async fn touching_a_deal_clears_stale() {
    let app = TestApp::new().await;
    let id = app
        .create_opportunity(json!({"company": "Revived", "last_touch_at": days_ago(30)}))
        .await;
    let (_, alerts) = app.get("/alerts/stale").await;
    assert_eq!(companies(&alerts), vec!["Revived"]);

    app.patch(
        &format!("/opportunities/{id}"),
        json!({"last_touch_at": days_ago(0)}),
    )
    .await;
    let (_, alerts) = app.get("/alerts/stale").await;
    assert_eq!(alerts, json!([]));
}
