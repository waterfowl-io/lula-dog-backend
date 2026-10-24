//! The xAI extractor against a local fake of the chat completions endpoint.
//! No key, no network beyond loopback.

#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a failing assertion aborting the test is the mechanism"
)]

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use lula_dog_backend::extractor::{ExtractError, ExtractionInput, Extractor, GrokExtractor};
use lula_dog_backend::opportunities::{Opportunity, Stage};
use serde_json::{Value, json};

#[derive(Clone)]
struct Fake {
    reply: (StatusCode, Value),
    seen: Arc<Mutex<Option<(HeaderMap, Value)>>>,
}

async fn completions(
    State(fake): State<Fake>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    *fake.seen.lock().unwrap() = Some((headers, body));
    (fake.reply.0, Json(fake.reply.1))
}

/// Serve the fake on an ephemeral port; returns its base URL.
async fn serve(fake: Fake) -> String {
    let app = Router::new()
        .route("/v1/chat/completions", post(completions))
        .with_state(fake);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    format!("http://{addr}/v1")
}

fn opportunity() -> Opportunity {
    Opportunity {
        id: 7,
        company: "Rivet Box Co".to_owned(),
        contact_name: None,
        contact_email: None,
        value_cents: Some(100),
        stage: Stage::Qualified,
        next_step: None,
        last_touch_at: None,
        created_at: "2026-10-01 00:00:00".to_owned(),
        updated_at: "2026-10-01 00:00:00".to_owned(),
    }
}

fn completion(content: &Value) -> Value {
    json!({
        "id": "cmpl-1",
        "object": "chat.completion",
        "model": "grok-test",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content.to_string()},
            "finish_reason": "stop",
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1},
    })
}

async fn extract(fake: Fake) -> Result<lula_dog_backend::extractor::Extraction, ExtractError> {
    let base = serve(fake).await;
    let grok = GrokExtractor::new("test-key".to_owned(), "grok-test".to_owned(), &base).unwrap();
    let opp = opportunity();
    grok.extract(ExtractionInput {
        opportunity: &opp,
        text: "pasted thread",
        today: "2026-10-23",
    })
    .await
}

#[tokio::test]
async fn sends_a_structured_output_request_and_parses_the_reply() {
    let content = json!({
        "summary": "Customer accepted the quote.",
        "company": null,
        "contact_name": "Dana Ortiz",
        "contact_email": "dana@rivetbox.example",
        "value_cents": 1_245_000,
        "stage": "proposal",
        "next_step": "Send double-wall option",
        "latest_date": "2026-10-14T09:30:00-07:00",
        "tasks": [{"title": "Send pricing", "due_at": "2026-10-17"}],
    });
    let seen = Arc::new(Mutex::new(None));
    let got = extract(Fake {
        reply: (StatusCode::OK, completion(&content)),
        seen: seen.clone(),
    })
    .await
    .unwrap();

    assert_eq!(got.contact_name.as_deref(), Some("Dana Ortiz"));
    assert_eq!(got.value_cents, Some(1_245_000));
    assert_eq!(got.stage.as_deref(), Some("proposal"));
    assert_eq!(got.tasks.len(), 1);

    let (headers, body) = seen.lock().unwrap().clone().unwrap();
    assert_eq!(headers["authorization"], "Bearer test-key");
    assert_eq!(body["model"], "grok-test");
    assert_eq!(body["response_format"]["type"], "json_schema");
    assert_eq!(body["response_format"]["json_schema"]["strict"], true);
    let schema = &body["response_format"]["json_schema"]["schema"];
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["required"].as_array().unwrap().len(), 9);
    let user: Value =
        serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(user["today"], "2026-10-23");
    assert_eq!(user["text"], "pasted thread");
    assert_eq!(user["current_opportunity"]["company"], "Rivet Box Co");
}

#[tokio::test]
async fn http_errors_are_reported_with_status() {
    let err = extract(Fake {
        reply: (
            StatusCode::UNAUTHORIZED,
            json!({"error": "invalid api key"}),
        ),
        seen: Arc::default(),
    })
    .await
    .unwrap_err();
    assert!(
        matches!(err, ExtractError::Status { status: 401, .. }),
        "{err}"
    );
}

#[tokio::test]
async fn off_schema_content_is_malformed() {
    let err = extract(Fake {
        reply: (
            StatusCode::OK,
            completion(&json!({"summary": "x", "surprise": true})),
        ),
        seen: Arc::default(),
    })
    .await
    .unwrap_err();
    assert!(matches!(err, ExtractError::Malformed(_)), "{err}");
}

#[tokio::test]
async fn unreachable_server_is_a_request_error() {
    // Bind then drop, so the port is very likely closed.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let grok = GrokExtractor::new(
        "test-key".to_owned(),
        "grok-test".to_owned(),
        &format!("http://{addr}/v1"),
    )
    .unwrap();
    let opp = opportunity();
    let err = grok
        .extract(ExtractionInput {
            opportunity: &opp,
            text: "x",
            today: "2026-10-23",
        })
        .await
        .unwrap_err();
    assert!(matches!(err, ExtractError::Request(_)), "{err}");
}
