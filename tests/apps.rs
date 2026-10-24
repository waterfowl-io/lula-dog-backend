//! App definitions: create, version, publish, rollback, and the semantic
//! validation that rejects a definition before it's ever stored. Gated on
//! `TEST_CONTROL_DATABASE_URL`: skips with a printed note when it isn't set.

#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a failing assertion aborting the test is the mechanism"
)]

mod common;

use axum::http::StatusCode;
use common::TestApp;
use serde_json::json;

async fn create_a_saved_query(app: &TestApp, key: &str) {
    let (status, body) = app
        .post(
            "/queries",
            json!({
                "key": key,
                "name": key,
                "description": null,
                "sql_text": "SELECT 1 AS value",
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

fn minimal_definition(query_key: &str) -> serde_json::Value {
    json!({
        "version": 1,
        "pages": [{
            "id": "home",
            "path": "/",
            "title": "Home",
            "layout": {"type": "component", "component_id": "metric"},
            "components": [{
                "id": "metric",
                "type": "metric_card",
                "title": "A metric",
                "query": {"query": query_key, "params": {}},
                "action": null,
                "props": {"value_field": "value"}
            }]
        }]
    })
}

#[tokio::test]
async fn create_app_version_publish_and_fetch() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    create_a_saved_query(&app, "a_metric").await;

    let (status, created) = app
        .post("/apps", json!({"key": "dash", "name": "Dashboard"}))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let app_id = created["id"].as_str().unwrap();

    let (status, version) = app
        .post(
            &format!("/apps/{app_id}/versions"),
            json!({"definition": minimal_definition("a_metric")}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{version}");
    assert_eq!(version["version"], 1);
    let version_id = version["id"].as_str().unwrap();

    let (status, published) = app
        .post(
            &format!("/apps/{app_id}/versions/{version_id}/publish"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{published}");
    assert_eq!(published["published_version_id"], version_id);

    let (status, detail) = app.get(&format!("/apps/{app_id}")).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["version"]["id"], version_id);
}

#[tokio::test]
async fn duplicate_app_key_is_a_conflict() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let (status, _) = app
        .post("/apps", json!({"key": "dup", "name": "One"}))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) = app
        .post("/apps", json!({"key": "dup", "name": "Two"}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[tokio::test]
async fn version_referencing_an_unknown_saved_query_is_rejected() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let (status, created) = app
        .post("/apps", json!({"key": "bad", "name": "Bad"}))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let app_id = created["id"].as_str().unwrap();

    let (status, body) = app
        .post(
            &format!("/apps/{app_id}/versions"),
            json!({"definition": minimal_definition("does_not_exist")}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn version_with_duplicate_component_ids_is_rejected() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    create_a_saved_query(&app, "dup_metric").await;
    let (status, created) = app
        .post("/apps", json!({"key": "dupcomp", "name": "Dup"}))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let app_id = created["id"].as_str().unwrap();

    let mut definition = minimal_definition("dup_metric");
    let component = definition["pages"][0]["components"][0].clone();
    definition["pages"][0]["components"]
        .as_array_mut()
        .unwrap()
        .push(component);

    let (status, body) = app
        .post(
            &format!("/apps/{app_id}/versions"),
            json!({"definition": definition}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn rollback_creates_a_new_version_and_republishes() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    create_a_saved_query(&app, "rb_metric").await;
    let (_, created) = app
        .post("/apps", json!({"key": "rollback", "name": "Rollback"}))
        .await;
    let app_id = created["id"].as_str().unwrap();

    let (_, v1) = app
        .post(
            &format!("/apps/{app_id}/versions"),
            json!({"definition": minimal_definition("rb_metric")}),
        )
        .await;
    assert_eq!(v1["version"], 1);
    app.post(
        &format!(
            "/apps/{app_id}/versions/{}/publish",
            v1["id"].as_str().unwrap()
        ),
        json!({}),
    )
    .await;

    let mut v2_def = minimal_definition("rb_metric");
    v2_def["pages"][0]["title"] = json!("Changed");
    let (_, v2) = app
        .post(
            &format!("/apps/{app_id}/versions"),
            json!({"definition": v2_def}),
        )
        .await;
    assert_eq!(v2["version"], 2);
    let (status, published) = app
        .post(
            &format!(
                "/apps/{app_id}/versions/{}/publish",
                v2["id"].as_str().unwrap()
            ),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{published}");

    // Roll back to version 1: a new version 3 is created (copying v1's
    // definition) and published, not a pointer move back to v1 itself.
    let (status, rolled_back) = app
        .post(&format!("/apps/{app_id}/rollback"), json!({"version": 1}))
        .await;
    assert_eq!(status, StatusCode::OK, "{rolled_back}");
    assert_eq!(rolled_back["version"], 3);
    assert_eq!(rolled_back["definition"]["pages"][0]["title"], "Home");

    let (status, detail) = app.get(&format!("/apps/{app_id}")).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["version"]["version"], 3);
}
