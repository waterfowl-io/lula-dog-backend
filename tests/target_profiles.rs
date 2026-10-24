//! CRUD for target profiles.

#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a failing assertion aborting the test is the mechanism"
)]

mod common;

use axum::http::StatusCode;
use common::TestApp;
use serde_json::json;

#[tokio::test]
async fn create_get_list_patch_and_delete() {
    let app = TestApp::new().await;

    let (status, created) = app
        .post(
            "/target-profiles",
            json!({
                "name": "Jack: IL packaging",
                "industries": ["foods", "brewing"],
                "regions": ["IL"],
                "include_keywords": ["coffee"],
                "exclude_keywords": ["law firm"],
                "min_fit_score": 60,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["name"], "Jack: IL packaging");
    assert_eq!(created["industries"], json!(["foods", "brewing"]));
    assert_eq!(created["min_fit_score"], 60);
    let id = created["id"].as_i64().unwrap();

    let (status, got) = app.get(&format!("/target-profiles/{id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got["regions"], json!(["IL"]));

    let (status, list) = app.get("/target-profiles").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);

    let (status, patched) = app
        .patch(
            &format!("/target-profiles/{id}"),
            json!({"min_fit_score": 75, "exclude_keywords": ["bank"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["min_fit_score"], 75);
    assert_eq!(patched["exclude_keywords"], json!(["bank"]));
    // Untouched fields survive the patch.
    assert_eq!(patched["name"], "Jack: IL packaging");

    let (status, _) = app.delete(&format!("/target-profiles/{id}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = app.get(&format!("/target-profiles/{id}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn name_cannot_be_blank() {
    let app = TestApp::new().await;
    let (status, body) = app.post("/target-profiles", json!({"name": "   "})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn seed_default_is_idempotent_and_matches_jack() {
    let app = TestApp::new().await;
    let mut conn = app.pool.acquire().await.unwrap();
    lula_dog_backend::target_profiles::seed_default(&mut conn, 55)
        .await
        .unwrap();
    lula_dog_backend::target_profiles::seed_default(&mut conn, 55)
        .await
        .unwrap();

    let (status, list) = app.get("/target-profiles").await;
    assert_eq!(status, StatusCode::OK);
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 1, "seeding twice must not duplicate");
    assert_eq!(list[0]["min_fit_score"], 55);
    assert_eq!(list[0]["regions"], json!(["IL"]));
}
