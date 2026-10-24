//! CRUD for opportunities and tasks, plus error shapes.

#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a failing assertion aborting the test is the mechanism"
)]

mod common;

use axum::http::StatusCode;
use common::TestApp;
use serde_json::json;

/// (field, old_value, new_value, status, entry_id)
type AuditRow = (String, Option<String>, Option<String>, String, Option<i64>);

#[tokio::test]
async fn health_and_openapi() {
    let app = TestApp::new().await;
    let (status, body) = app.get("/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"status": "ok"}));

    let (status, spec) = app.get("/openapi.json").await;
    assert_eq!(status, StatusCode::OK);
    for path in [
        "/health",
        "/opportunities",
        "/opportunities/{id}",
        "/opportunities/{id}/tasks",
        "/opportunities/{id}/paste",
        "/opportunities/{id}/proposals/accept-all",
        "/opportunities/{id}/proposals/dismiss-all",
        "/tasks/{id}",
        "/proposals/{id}/accept",
        "/proposals/{id}/reject",
        "/alerts/stale",
    ] {
        assert!(spec["paths"][path].is_object(), "missing {path} in spec");
    }
}

#[tokio::test]
async fn create_get_list_and_filter_by_stage() {
    let app = TestApp::new().await;
    let (status, created) = app
        .post(
            "/opportunities",
            json!({
                "company": "  Acme Mailers  ",
                "contact_email": "buyer@acme.example",
                "value_cents": 125_000,
                "last_touch_at": "2026-10-20T15:00:00-04:00",
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["company"], "Acme Mailers");
    assert_eq!(created["stage"], "lead");
    assert_eq!(created["value_cents"], 125_000);
    assert_eq!(created["last_touch_at"], "2026-10-20 19:00:00");
    assert_eq!(created["contact_name"], json!(null));
    assert_eq!(created["open_task_count"], 0);
    let id = created["id"].as_i64().unwrap();

    app.create_opportunity(json!({"company": "Pallet Co", "stage": "proposal"}))
        .await;

    let (status, all) = app.get("/opportunities").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(all.as_array().unwrap().len(), 2);

    let (_, proposals) = app.get("/opportunities?stage=proposal").await;
    let proposals = proposals.as_array().unwrap();
    assert_eq!(proposals.len(), 1);
    assert_eq!(proposals[0]["company"], "Pallet Co");

    let (status, detail) = app.get(&format!("/opportunities/{id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["opportunity"]["id"], id);
    assert_eq!(detail["timeline"], json!([]));
    assert_eq!(detail["pending_proposals"], json!([]));
    assert_eq!(detail["tasks"], json!([]));
}

#[tokio::test]
async fn validation_errors_are_json_400s() {
    let app = TestApp::new().await;
    for body in [
        json!({"company": "X", "stage": "negotiation"}),
        json!({"company": "   "}),
        json!({"company": "X", "value_cents": -5}),
        json!({"company": "X", "contact_email": "not an email"}),
        json!({"company": "X", "last_touch_at": "yesterday"}),
        json!({"company": "X", "reorder_interval_days": 30}),
        json!({"contact_name": "no company"}),
    ] {
        let (status, err) = app.post("/opportunities", body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(err["error"]["code"], "bad_request", "{body}");
    }

    let (status, err) = app.get("/opportunities?stage=bogus").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(err["error"]["code"], "bad_request");

    let (status, err) = app.get("/opportunities/abc").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(err["error"]["code"], "bad_request");

    let (status, err) = app.get("/opportunities/999").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(err["error"]["code"], "not_found");
}

#[tokio::test]
async fn stage_is_enforced_by_the_schema_too() {
    let app = TestApp::new().await;
    let result = sqlx::query("INSERT INTO opportunities (company, stage) VALUES ('X', 'nope')")
        .execute(&app.pool)
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn patch_updates_clears_and_audits() {
    let app = TestApp::new().await;
    let id = app
        .create_opportunity(json!({
            "company": "Acme",
            "contact_name": "Lee",
            "next_step": "Call",
        }))
        .await;

    let (status, updated) = app
        .patch(
            &format!("/opportunities/{id}"),
            json!({"stage": "qualified", "value_cents": 50_000, "next_step": null}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["stage"], "qualified");
    assert_eq!(updated["value_cents"], 50_000);
    assert_eq!(updated["next_step"], json!(null));
    // Absent means untouched.
    assert_eq!(updated["contact_name"], "Lee");

    // Each changed field left an accepted, entry-less audit row.
    let rows: Vec<AuditRow> = sqlx::query_as(
        "SELECT field, old_value, new_value, status, entry_id FROM field_changes
         WHERE opportunity_id = ?1 ORDER BY id",
    )
    .bind(id)
    .fetch_all(&app.pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            (
                "value_cents".to_owned(),
                None,
                Some("50000".to_owned()),
                "accepted".to_owned(),
                None
            ),
            (
                "stage".to_owned(),
                Some("lead".to_owned()),
                Some("qualified".to_owned()),
                "accepted".to_owned(),
                None
            ),
            (
                "next_step".to_owned(),
                Some("Call".to_owned()),
                None,
                "accepted".to_owned(),
                None
            ),
        ]
    );

    // A no-op patch writes no audit rows.
    let (status, _) = app
        .patch(
            &format!("/opportunities/{id}"),
            json!({"stage": "qualified"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM field_changes")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(count, 3);

    // Fields that cannot be cleared refuse an explicit null.
    for body in [json!({"company": null}), json!({"stage": null})] {
        let (status, err) = app
            .patch(&format!("/opportunities/{id}"), body.clone())
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(err["error"]["code"], "bad_request");
    }
    // A failing patch applies nothing, even fields listed before the bad one.
    let (status, _) = app
        .patch(
            &format!("/opportunities/{id}"),
            json!({"contact_name": "Changed", "stage": "bogus"}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = app
        .patch(
            &format!("/opportunities/{id}"),
            json!({"contact_name": "Changed", "contact_email": "nope"}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (_, detail) = app.get(&format!("/opportunities/{id}")).await;
    assert_eq!(detail["opportunity"]["contact_name"], "Lee");

    let (status, _) = app
        .patch("/opportunities/999", json!({"stage": "won"}))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn task_crud() {
    let app = TestApp::new().await;
    let id = app.create_opportunity(json!({"company": "Acme"})).await;

    let (status, task) = app
        .post(
            &format!("/opportunities/{id}/tasks"),
            json!({"title": "Send box samples", "due_at": "2026-11-02"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{task}");
    assert_eq!(task["title"], "Send box samples");
    assert_eq!(task["due_at"], "2026-11-02 00:00:00");
    assert_eq!(task["done"], false);
    assert_eq!(task["source_entry_id"], json!(null));
    let task_id = task["id"].as_i64().unwrap();

    let (_, view) = app.get(&format!("/opportunities/{id}")).await;
    assert_eq!(view["opportunity"]["open_task_count"], 1);
    assert_eq!(view["tasks"].as_array().unwrap().len(), 1);

    let (status, task) = app
        .patch(
            &format!("/tasks/{task_id}"),
            json!({"done": true, "due_at": null}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(task["done"], true);
    assert_eq!(task["due_at"], json!(null));
    assert_eq!(task["title"], "Send box samples");

    let (_, view) = app.get(&format!("/opportunities/{id}")).await;
    assert_eq!(view["opportunity"]["open_task_count"], 0);

    for body in [
        json!({"title": null}),
        json!({"done": null}),
        json!({"title": " "}),
    ] {
        let (status, _) = app.patch(&format!("/tasks/{task_id}"), body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }

    let (status, _) = app.delete(&format!("/tasks/{task_id}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = app.delete(&format!("/tasks/{task_id}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = app
        .patch(&format!("/tasks/{task_id}"), json!({"done": true}))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = app
        .post(&format!("/opportunities/{id}/tasks"), json!({"title": " "}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = app
        .post("/opportunities/999/tasks", json!({"title": "x"}))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
