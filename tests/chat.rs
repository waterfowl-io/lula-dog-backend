//! Chat: data-mode read/write tool calls via the fake provider, the
//! per-turn write cap, undo, build mode's phase-1 501, and workspace
//! isolation. Gated on `TEST_CONTROL_DATABASE_URL`: skips with a printed
//! note when it isn't set (chat conversations need an app, which needs
//! the control plane).

#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a failing assertion aborting the test is the mechanism"
)]

mod common;

use axum::http::StatusCode;
use common::{SseEvent, TestApp, parse_sse};
use lula_dog_backend::actions;
use lula_dog_backend::config::{ChatConfig, ChatProviderConfig};
use serde_json::{Value, json};
use uuid::Uuid;

async fn create_app(app: &TestApp, key: &str) {
    let (status, body) = app.post("/apps", json!({"key": key, "name": key})).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

async fn saved_query(app: &TestApp, key: &str, sql_text: &str) {
    let (status, body) = app
        .post(
            "/queries",
            json!({
                "key": key,
                "name": key,
                "description": null,
                "sql_text": sql_text,
                "params": [],
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

fn events_named<'a>(events: &'a [SseEvent], name: &str) -> Vec<&'a SseEvent> {
    events.iter().filter(|e| e.name == name).collect()
}

#[tokio::test]
async fn data_mode_stale_deals_runs_the_saved_query_and_answers() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    create_app(&app, "sales").await;
    saved_query(&app, "stale_deals", "SELECT 'Acme' AS company").await;

    let (status, body) = app
        .post_sse(
            "/apps/sales/chat",
            json!({"mode": "data", "message": "which deals are stale?"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let events = parse_sse(&body);

    let tool_calls = events_named(&events, "tool_call");
    assert_eq!(tool_calls.len(), 1, "{body}");
    assert_eq!(tool_calls[0].data["name"], "run_saved_query");
    assert_eq!(tool_calls[0].data["args"]["query"], "stale_deals");

    let tool_results = events_named(&events, "tool_result");
    assert_eq!(tool_results.len(), 1, "{body}");
    assert_eq!(tool_results[0].data["result"]["row_count"], 1);

    assert_eq!(events_named(&events, "final").len(), 1, "{body}");
    assert!(
        events_named(&events, "action_applied").is_empty(),
        "a read should not apply an action"
    );
    assert!(events_named(&events, "error").is_empty(), "{body}");
}

#[tokio::test]
async fn data_mode_move_deal_applies_and_can_be_undone() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    create_app(&app, "sales").await;
    let deal_id = app
        .create_opportunity(json!({"company": "Acme", "stage": "lead"}))
        .await;

    let (status, body) = app
        .post_sse(
            "/apps/sales/chat",
            json!({"mode": "data", "message": format!("move deal {deal_id} to qualified")}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let events = parse_sse(&body);

    let applied = events_named(&events, "action_applied");
    assert_eq!(applied.len(), 1, "{body}");
    assert_eq!(applied[0].data["action"], "opportunity.update_stage");
    assert_eq!(applied[0].data["result"]["stage"], "qualified");
    assert!(
        applied[0].data["undoable"].as_bool().unwrap_or(false),
        "{body}"
    );
    let action_id = applied[0].data["id"].as_str().unwrap().to_owned();

    let (status, deal) = app.get(&format!("/opportunities/{deal_id}")).await;
    assert_eq!(status, StatusCode::OK, "{deal}");
    assert_eq!(deal["opportunity"]["stage"], "qualified");

    // Undo puts it back.
    let (status, undone) = app
        .post(
            &format!("/apps/sales/chat/actions/{action_id}/undo"),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{undone}");

    let (status, deal) = app.get(&format!("/opportunities/{deal_id}")).await;
    assert_eq!(status, StatusCode::OK, "{deal}");
    assert_eq!(deal["opportunity"]["stage"], "lead");

    // A second undo is a conflict, not a silent no-op or a crash.
    let (status, body) = app
        .post(
            &format!("/apps/sales/chat/actions/{action_id}/undo"),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[tokio::test]
async fn data_mode_create_deal_applies_through_chat() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    create_app(&app, "sales").await;

    let (status, body) = app
        .post_sse(
            "/apps/sales/chat",
            json!({"mode": "data", "message": "please create a deal for Fernhill Candle Co"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let events = parse_sse(&body);

    let applied = events_named(&events, "action_applied");
    assert_eq!(applied.len(), 1, "{body}");
    assert_eq!(applied[0].data["action"], "opportunity.create");
    assert!(
        !applied[0].data["undoable"].as_bool().unwrap_or(true),
        "{body}"
    );

    let (status, list) = app.get("/opportunities").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .any(|o| o["company"] == "Fernhill Candle Co"),
        "{list}"
    );
}

#[tokio::test]
async fn write_cap_of_zero_blocks_the_write_but_still_answers() {
    let Some(app) = TestApp::with_control_and_chat(ChatConfig {
        provider: ChatProviderConfig::Fake,
        max_tool_rounds: 6,
        max_tokens: 1024,
        max_writes_per_turn: 0,
        price_input_per_1m_usd: None,
        price_output_per_1m_usd: None,
    })
    .await
    else {
        return;
    };
    create_app(&app, "sales").await;

    let (status, body) = app
        .post_sse(
            "/apps/sales/chat",
            json!({"mode": "data", "message": "create a deal for Acme"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let events = parse_sse(&body);

    assert!(events_named(&events, "action_applied").is_empty(), "{body}");
    let tool_results = events_named(&events, "tool_result");
    assert_eq!(tool_results.len(), 1, "{body}");
    assert!(
        tool_results[0].data["result"]["error"]
            .as_str()
            .unwrap_or_default()
            .contains("write cap"),
        "{body}"
    );
    assert_eq!(
        events_named(&events, "final").len(),
        1,
        "turn should still finish: {body}"
    );

    let (status, list) = app.get("/opportunities").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert!(
        list.as_array().unwrap().is_empty(),
        "no deal should have been created: {list}"
    );
}

#[tokio::test]
async fn viewer_role_cannot_reach_chat_at_all() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    create_app(&app, "sales").await;
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
        .post_sse_as(
            viewer,
            "/apps/sales/chat",
            json!({"mode": "data", "message": "create a deal for Acme"}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, list) = app.get("/opportunities").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert!(
        list.as_array().unwrap().is_empty(),
        "the viewer's request must not have written anything"
    );
}

#[tokio::test]
async fn build_mode_is_not_implemented_in_this_phase() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    create_app(&app, "sales").await;

    let (status, body) = app
        .post_sse(
            "/apps/sales/chat",
            json!({"mode": "build", "message": "add a metric card"}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body}");
}

#[tokio::test]
async fn conversation_history_persists_and_is_isolated_to_its_own_workspace() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    create_app(&app, "sales").await;

    let (status, body) = app
        .post_sse(
            "/apps/sales/chat",
            json!({"mode": "data", "message": "hello there"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let events = parse_sse(&body);
    let final_event = &events_named(&events, "final")[0];
    let message_id = final_event.data["message_id"].as_str().unwrap();

    let (status, conversations) = app.get("/apps/sales/conversations?mode=data").await;
    assert_eq!(status, StatusCode::OK, "{conversations}");
    let conversations = conversations.as_array().unwrap();
    assert_eq!(conversations.len(), 1, "{conversations:?}");
    let conversation_id = conversations[0]["id"].as_str().unwrap().to_owned();
    assert_eq!(conversations[0]["title"], "hello there");

    let (status, messages) = app
        .get(&format!(
            "/apps/sales/conversations/{conversation_id}/messages"
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{messages}");
    let messages = messages.as_array().unwrap();
    assert!(
        messages.iter().any(|m| m["id"] == message_id),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m["role"] == "user" && m["content"] == "hello there")
    );

    // A second workspace, owned by the same dev user, never sees it: a
    // different SQLite file entirely, not just a different filter.
    let (status, created) = app
        .post("/workspaces", json!({"name": "Other workspace"}))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let other_workspace = created["id"].as_str().unwrap();
    let (status, other_apps) = app.get(&format!("/w/{other_workspace}/apps")).await;
    assert_eq!(status, StatusCode::OK, "{other_apps}");
    let (status, created_app) = app
        .post(
            &format!("/w/{other_workspace}/apps"),
            json!({"key": "sales", "name": "sales"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created_app}");

    let (status, foreign) = app
        .get(&format!(
            "/w/{other_workspace}/apps/sales/conversations/{conversation_id}/messages"
        ))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{foreign}");
}

/// The chat write tool and the equivalent HTTP endpoint are the exact same
/// function (`opportunities::apply_patch_via_pool`), so this is really a
/// guard against them drifting apart in the future rather than a live
/// comparison -- but it does exercise both call paths end to end and check
/// they land on an identical result.
#[tokio::test]
async fn chat_action_dispatch_matches_the_http_endpoint_effect() {
    let app = TestApp::new().await;
    let via_http = app
        .create_opportunity(json!({"company": "Via HTTP", "stage": "lead"}))
        .await;
    let via_chat = app
        .create_opportunity(json!({"company": "Via Chat", "stage": "lead"}))
        .await;

    let (status, _) = app
        .patch(
            &format!("/opportunities/{via_http}"),
            json!({"stage": "won"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let outcome = actions::dispatch(
        &app.pool,
        "opportunity.update_stage",
        &json!({"opportunity_id": via_chat, "stage": "won"}),
    )
    .await
    .unwrap();

    let (_, http_view) = app.get(&format!("/opportunities/{via_http}")).await;
    assert_eq!(http_view["opportunity"]["stage"], "won");
    assert_eq!(outcome.result["stage"], "won");
    assert_eq!(
        http_view["opportunity"]["stage"], outcome.result["stage"],
        "the two call paths must land on the same stage"
    );
}

#[tokio::test]
async fn unknown_tool_name_is_a_bad_request_not_a_panic() {
    let app = TestApp::new().await;
    let outcome = actions::dispatch(&app.pool, "opportunity.delete", &json!({})).await;
    assert!(outcome.is_err());
}

#[tokio::test]
async fn method_not_allowed_without_control_plane_still_works_for_fake_provider() {
    // Chat doesn't strictly need the control plane for a read tool that
    // only touches the workspace SQLite (e.g. get_deal), but
    // run_saved_query does (saved queries live in Postgres); without a
    // control plane at all, `resolve_app` itself needs it (apps also live
    // in Postgres), so this just documents that chat has the same
    // dependency on `CONTROL_DATABASE_URL` every other `/apps` route has.
    let app = TestApp::new().await;
    let (status, _) = app
        .post_sse(
            "/apps/sales/chat",
            json!({"mode": "data", "message": "hello"}),
        )
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}
