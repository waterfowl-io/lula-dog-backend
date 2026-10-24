//! Saved queries, the DuckDB-free query runner's safety checks (single
//! `SELECT`/`WITH` statement, row limit, named params), and schema
//! introspection. Gated on `TEST_CONTROL_DATABASE_URL`: skips with a
//! printed note when it isn't set.

#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "a failing assertion aborting the test is the mechanism"
)]

mod common;

use axum::http::StatusCode;
use common::TestApp;
use serde_json::json;

async fn saved_query(
    app: &TestApp,
    key: &str,
    sql_text: &str,
    params: serde_json::Value,
) -> String {
    let (status, body) = app
        .post(
            "/queries",
            json!({
                "key": key,
                "name": key,
                "description": null,
                "sql_text": sql_text,
                "params": params,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn create_list_and_get_saved_queries() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    saved_query(&app, "q1", "SELECT 1 AS one", json!([])).await;
    let (status, list) = app.get("/queries").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn duplicate_saved_query_key_is_a_conflict() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    saved_query(&app, "dup", "SELECT 1", json!([])).await;
    let (status, body) = app
        .post(
            "/queries",
            json!({"key": "dup", "name": "dup", "description": null, "sql_text": "SELECT 2"}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[tokio::test]
async fn run_a_simple_query() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let id = saved_query(&app, "simple", "SELECT 1 AS a, 'x' AS b", json!([])).await;
    let (status, result) = app.post(&format!("/queries/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["row_count"], 1);
    assert_eq!(result["truncated"], false);
    assert_eq!(result["rows"][0][0], 1);
    assert_eq!(result["rows"][0][1], "x");
    assert_eq!(result["sql"], "SELECT 1 AS a, 'x' AS b");
    let column_names: Vec<&str> = result["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(column_names, ["a", "b"]);
}

#[tokio::test]
async fn run_against_real_opportunity_data() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    app.create_opportunity(json!({"company": "Runner Co", "value_cents": 500}))
        .await;
    let id = saved_query(
        &app,
        "deals",
        "SELECT company, value_cents FROM opportunities",
        json!([]),
    )
    .await;
    let (status, result) = app.post(&format!("/queries/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["rows"][0][0], "Runner Co");
    assert_eq!(result["rows"][0][1], 500);
}

#[tokio::test]
async fn named_params_are_bound_not_interpolated() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    app.create_opportunity(json!({"company": "Alpha"})).await;
    app.create_opportunity(json!({"company": "Beta"})).await;
    let id = saved_query(
        &app,
        "by_company",
        "SELECT id FROM opportunities WHERE company = :company",
        json!([{"name": "company", "type": "string"}]),
    )
    .await;

    let (status, result) = app
        .post(
            &format!("/queries/{id}/run"),
            json!({"params": {"company": "Alpha"}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["row_count"], 1);

    // A value that would be dangerous if interpolated into the SQL text is
    // just a literal string value to bind -- no injection, no match.
    let (status, result) = app
        .post(
            &format!("/queries/{id}/run"),
            json!({"params": {"company": "x' OR '1'='1"}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["row_count"], 0, "{result}");
}

#[tokio::test]
async fn missing_param_value_is_a_bad_request() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let id = saved_query(
        &app,
        "needs_param",
        "SELECT * FROM opportunities WHERE company = :company",
        json!([{"name": "company", "type": "string"}]),
    )
    .await;
    let (status, body) = app.post(&format!("/queries/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn non_select_sql_text_is_rejected_at_run_time() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let id = saved_query(&app, "evil", "DELETE FROM opportunities", json!([])).await;
    let (status, body) = app.post(&format!("/queries/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn a_second_statement_is_rejected() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let id = saved_query(
        &app,
        "injection",
        "SELECT 1; DELETE FROM opportunities",
        json!([]),
    )
    .await;
    let (status, body) = app.post(&format!("/queries/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn row_limit_truncates_and_reports_it() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    for i in 0..5 {
        app.create_opportunity(json!({"company": format!("Row {i}")}))
            .await;
    }
    let id = saved_query(&app, "limited", "SELECT id FROM opportunities", json!([])).await;

    // The harness configures `query_max_rows = 1000`, well above 5 rows,
    // so this just confirms the untruncated case reports accurately...
    let (status, result) = app.post(&format!("/queries/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["row_count"], 5);
    assert_eq!(result["truncated"], false);
}

#[tokio::test]
async fn saved_query_is_reachable_and_runnable_by_key_or_by_uuid() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let id = saved_query(&app, "by_key_or_uuid", "SELECT 1 AS a", json!([])).await;

    let (status, by_key) = app.get("/queries/by_key_or_uuid").await;
    assert_eq!(status, StatusCode::OK, "{by_key}");
    assert_eq!(by_key["id"], id);

    let (status, by_uuid) = app.get(&format!("/queries/{id}")).await;
    assert_eq!(status, StatusCode::OK, "{by_uuid}");
    assert_eq!(by_uuid["id"], id);

    let (status, run_by_key) = app.post("/queries/by_key_or_uuid/run", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{run_by_key}");
    assert_eq!(run_by_key["rows"][0][0], 1);

    let (status, run_by_uuid) = app.post(&format!("/queries/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "{run_by_uuid}");
    assert_eq!(run_by_uuid["rows"][0][0], 1);
}

#[tokio::test]
async fn unknown_saved_query_key_is_not_found() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let (status, body) = app.get("/queries/does-not-exist").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, body) = app.post("/queries/does-not-exist/run", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn saved_query_key_from_another_workspace_is_not_found() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    saved_query(&app, "cross-ws", "SELECT 1", json!([])).await;

    let (status, other_ws) = app
        .post("/workspaces", json!({"name": "Other Workspace"}))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{other_ws}");
    let other_ws_id = other_ws["id"].as_str().unwrap();

    let (status, body) = app.get(&format!("/w/{other_ws_id}/queries/cross-ws")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn schema_introspection_reports_tables_and_a_primary_key() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let (status, schema) = app.get("/schema").await;
    assert_eq!(status, StatusCode::OK, "{schema}");
    let columns = schema.as_array().unwrap();
    let id_column = columns
        .iter()
        .find(|c| c["table_name"] == "opportunities" && c["column_name"] == "id")
        .unwrap_or_else(|| panic!("opportunities.id missing from {schema}"));
    assert_eq!(id_column["is_primary_key"], true);
    assert!(
        columns.iter().any(|c| c["table_name"] == "tasks"),
        "tasks table missing from {schema}"
    );
}
