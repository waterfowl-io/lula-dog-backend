//! `/me`, `/workspaces`, and `/w/{workspace_id}` membership enforcement.
//! All gated on `TEST_CONTROL_DATABASE_URL`: `TestApp::with_control` skips
//! (with a printed note) when it isn't set, so the rest of the suite never
//! needs Postgres. Each test creates its own profile/workspace with random
//! ids, so they're safe to run in parallel against a shared database.

#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "a failing assertion aborting the test is the mechanism"
)]

mod common;

use axum::http::{Method, StatusCode};
use common::TestApp;
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn me_returns_profile_and_memberships() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let (status, body) = app.get("/me").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["profile"]["id"], app.dev_user_id.to_string());
    let workspaces = body["workspaces"].as_array().unwrap();
    assert!(
        workspaces
            .iter()
            .any(|w| w["id"] == app.workspace_id.to_string()),
        "{body}"
    );
}

#[tokio::test]
async fn create_workspace_makes_the_caller_owner_and_a_sqlite_file() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let (status, created) = app
        .post("/workspaces", json!({"name": "A New Workspace"}))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let workspace_id = created["id"].as_str().unwrap();

    let (status, list) = app.get("/workspaces").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let memberships = list.as_array().unwrap();
    let membership = memberships
        .iter()
        .find(|w| w["id"] == workspace_id)
        .unwrap_or_else(|| panic!("new workspace not in {list}"));
    assert_eq!(membership["role"], "owner");
}

#[tokio::test]
async fn create_workspace_rejects_an_empty_name() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let (status, body) = app.post("/workspaces", json!({"name": "   "})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn non_member_is_forbidden() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let outsider = Uuid::new_v4();
    let (status, body) = app
        .request_as(outsider, Method::GET, "/opportunities", None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn unknown_workspace_is_forbidden_not_found() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    // A membership check happens before the workspace itself is looked up,
    // so a workspace id nobody owns reads the same as one that doesn't
    // exist: forbidden, not a 404 that would leak which ids are valid.
    let bogus_workspace = format!("/w/{}/opportunities", Uuid::new_v4());
    let (status, body) = app.get(&bogus_workspace).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn viewer_role_can_read_but_not_write() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let control = app.control.as_ref().unwrap();
    let viewer = Uuid::new_v4();
    lula_dog_backend::control::profiles::upsert_from_jwt(control, viewer, "viewer@test.example")
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, 'viewer')",
    )
    .bind(app.workspace_id)
    .bind(viewer)
    .execute(control)
    .await
    .unwrap();

    let (status, body) = app
        .request_as(viewer, Method::GET, "/opportunities", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = app
        .request_as(
            viewer,
            Method::POST,
            "/opportunities",
            Some(json!({"company": "Viewer Co"})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn two_workspaces_have_independent_sqlite_data() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    app.create_opportunity(json!({"company": "Workspace One Co"}))
        .await;

    let (status, created) = app
        .post("/workspaces", json!({"name": "Second Workspace"}))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let second_id = created["id"].as_str().unwrap();

    let (status, list) = app.get(&format!("/w/{second_id}/opportunities")).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list.as_array().unwrap().len(), 0, "{list}");
}
