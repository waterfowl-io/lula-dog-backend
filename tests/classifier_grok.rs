//! The Grok classifier against a local fake of the chat completions
//! endpoint, same pattern as `tests/grok.rs`. No key, no real network.

#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a failing assertion aborting the test is the mechanism"
)]

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use lula_dog_backend::classifier::{
    Classification, ClassificationInput, Classifier, ClassifyError, GrokClassifier,
};
use lula_dog_backend::companies::{Company, FilterStatus};
use lula_dog_backend::target_profiles::TargetProfile;
use serde_json::{Value, json};

#[derive(Clone)]
struct Fake {
    reply: (StatusCode, Value),
    seen: Arc<Mutex<Option<Value>>>,
}

async fn completions(
    State(fake): State<Fake>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    *fake.seen.lock().unwrap() = Some(body);
    (fake.reply.0, Json(fake.reply.1))
}

async fn serve(fake: Fake) -> String {
    let app = Router::new()
        .route("/v1/chat/completions", post(completions))
        .with_state(fake);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    format!("http://{addr}/v1")
}

fn company() -> Company {
    Company {
        id: 1,
        name: "Acme Coffee Roasters".to_owned(),
        normalized_name: "acme coffee roasters".to_owned(),
        entity_type: None,
        jurisdiction: "IL".to_owned(),
        state_file_number: None,
        formation_date: None,
        status: None,
        principal_address: None,
        mailing_address: None,
        officers: None,
        registered_agent: None,
        website: Some("https://acme.example".to_owned()),
        domain: Some("acme.example".to_owned()),
        phone: None,
        category: None,
        filter_status: FilterStatus::Passed,
        filter_reason: None,
        pre_score: Some(10),
        created_at: "2026-01-01 00:00:00".to_owned(),
        updated_at: "2026-01-01 00:00:00".to_owned(),
    }
}

fn profile() -> TargetProfile {
    TargetProfile {
        id: 1,
        name: "test".to_owned(),
        industries: sqlx::types::Json(vec!["coffee".to_owned()]),
        regions: sqlx::types::Json(vec!["IL".to_owned()]),
        size_min: None,
        size_max: None,
        size_notes: None,
        include_keywords: sqlx::types::Json(vec![]),
        exclude_keywords: sqlx::types::Json(vec![]),
        min_fit_score: 50,
        created_at: "2026-01-01 00:00:00".to_owned(),
        updated_at: "2026-01-01 00:00:00".to_owned(),
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

async fn classify(fake: Fake) -> Result<Classification, ClassifyError> {
    let base = serve(fake).await;
    let grok = GrokClassifier::new("test-key".to_owned(), "grok-test".to_owned(), &base).unwrap();
    let company = company();
    let profile = profile();
    grok.classify(ClassificationInput {
        company: &company,
        profile: &profile,
        crawled_text: "We roast and ship coffee nationwide.",
        contact_hint: Some("https://acme.example/contact"),
    })
    .await
}

#[tokio::test]
async fn sends_a_structured_request_and_parses_the_reply() {
    let content = json!({
        "industry": "coffee roaster",
        "products": ["whole bean coffee"],
        "ships_physical_goods": true,
        "size_signals": ["12 employees"],
        "contact_page_url": "https://acme.example/contact",
        "fit_score": 85,
        "fit_reason": "Matches the coffee industry and ships physical goods.",
    });
    let seen = Arc::new(Mutex::new(None));
    let got = classify(Fake {
        reply: (StatusCode::OK, completion(&content)),
        seen: seen.clone(),
    })
    .await
    .unwrap();

    assert_eq!(got.industry.as_deref(), Some("coffee roaster"));
    assert_eq!(got.fit_score, 85);
    assert!(got.ships_physical_goods);
    assert_eq!(got.products, vec!["whole bean coffee".to_owned()]);

    let body = seen.lock().unwrap().clone().unwrap();
    assert_eq!(body["model"], "grok-test");
    assert_eq!(body["response_format"]["json_schema"]["strict"], true);
    let user: Value =
        serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(user["company"]["name"], "Acme Coffee Roasters");
    assert_eq!(user["target_profile"]["name"], "test");
    assert_eq!(user["crawled_text"], "We roast and ship coffee nationwide.");
}

#[tokio::test]
async fn out_of_range_fit_score_is_clamped() {
    let content = json!({
        "industry": "coffee roaster",
        "products": [],
        "ships_physical_goods": true,
        "size_signals": [],
        "contact_page_url": null,
        "fit_score": 500,
        "fit_reason": null,
    });
    let got = classify(Fake {
        reply: (StatusCode::OK, completion(&content)),
        seen: Arc::default(),
    })
    .await
    .unwrap();
    assert_eq!(got.fit_score, 100);
}

#[tokio::test]
async fn off_schema_content_is_malformed() {
    let err = classify(Fake {
        reply: (
            StatusCode::OK,
            completion(&json!({"industry": "x", "surprise": true})),
        ),
        seen: Arc::default(),
    })
    .await
    .unwrap_err();
    assert!(matches!(err, ClassifyError::Malformed(_)), "{err}");
}

#[tokio::test]
async fn http_errors_are_reported_with_status() {
    let err = classify(Fake {
        reply: (
            StatusCode::UNAUTHORIZED,
            json!({"error": "invalid api key"}),
        ),
        seen: Arc::default(),
    })
    .await
    .unwrap_err();
    assert!(
        matches!(err, ClassifyError::Status { status: 401, .. }),
        "{err}"
    );
}
