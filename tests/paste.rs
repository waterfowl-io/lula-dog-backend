//! The paste flow with the stub extractor, and the proposal lifecycle.

#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a failing assertion aborting the test is the mechanism"
)]

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::http::StatusCode;
use common::{TestApp, days_ago};
use lula_dog_backend::extractor::{ExtractError, Extraction, ExtractionInput, Extractor};
use serde_json::{Value, json};
use sqlx::SqlitePool;

const THREAD: &str = "\
From: Dana Ortiz <dana@rivetbox.example>
Date: Wed, 14 Oct 2026 09:30:00 -0700
Subject: Re: 12x10x8 RSC quote

Thanks for the quote. $12,450.00 for 5,000 units works for us.
Next step: Send double-wall option
TODO: Send double-wall pricing by 2026-10-17
TODO: Ask about pallet count
";

fn fields(changes: &Value) -> Vec<&str> {
    changes
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["field"].as_str().unwrap())
        .collect()
}

async fn paste(app: &TestApp, id: i64, text: &str) -> Value {
    let (status, body) = app
        .post(&format!("/opportunities/{id}/paste"), json!({"text": text}))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body
}

#[tokio::test]
async fn fills_empty_fields_and_proposes_over_existing_ones() {
    let app = TestApp::new().await;
    // value_cents and stage already set; contact and next_step empty.
    let id = app
        .create_opportunity(json!({
            "company": "Rivet Box Co",
            "value_cents": 1_000_000,
            "stage": "qualified",
            "last_touch_at": "2026-10-01 12:00:00",
        }))
        .await;

    let body = paste(&app, id, THREAD).await;

    let entry = &body["entry"];
    assert_eq!(entry["raw_text"], THREAD);
    assert_eq!(entry["summary"], "Re: 12x10x8 RSC quote");
    assert_eq!(entry["occurred_at"], "2026-10-14 16:30:00");
    let entry_id = entry["id"].as_i64().unwrap();

    // Empty fields auto-filled, plus last_touch_at moved forward.
    assert_eq!(
        fields(&body["auto_filled"]),
        vec![
            "contact_name",
            "contact_email",
            "next_step",
            "last_touch_at"
        ]
    );
    for change in body["auto_filled"].as_array().unwrap() {
        assert_eq!(change["status"], "accepted");
        assert_eq!(change["entry_id"], entry_id);
    }
    // Existing values only proposed.
    assert_eq!(fields(&body["proposals"]), vec!["value_cents", "stage"]);
    let value_proposal = &body["proposals"][0];
    assert_eq!(value_proposal["old_value"], "1000000");
    assert_eq!(value_proposal["new_value"], "1245000");
    assert_eq!(value_proposal["status"], "proposed");

    let tasks = body["suggested_tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0]["title"], "Send double-wall pricing");
    assert_eq!(tasks[0]["due_at"], "2026-10-17 00:00:00");
    for task in tasks {
        assert_eq!(task["done"], false);
        assert_eq!(task["source_entry_id"], entry_id);
    }

    // The opportunity: empty fields filled, existing ones NOT overwritten.
    let (_, detail) = app.get(&format!("/opportunities/{id}")).await;
    let opp = &detail["opportunity"];
    assert_eq!(opp["contact_name"], "Dana Ortiz");
    assert_eq!(opp["contact_email"], "dana@rivetbox.example");
    assert_eq!(opp["next_step"], "Send double-wall option");
    assert_eq!(opp["value_cents"], 1_000_000);
    assert_eq!(opp["stage"], "qualified");
    assert_eq!(opp["last_touch_at"], "2026-10-14 16:30:00");
    assert_eq!(opp["open_task_count"], 2);
    assert_eq!(
        fields(&detail["pending_proposals"]),
        vec!["value_cents", "stage"]
    );
    assert_eq!(detail["timeline"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn accept_applies_and_reject_does_not() {
    let app = TestApp::new().await;
    let id = app
        .create_opportunity(json!({
            "company": "Rivet Box Co", "value_cents": 1_000_000, "stage": "qualified",
        }))
        .await;
    let body = paste(&app, id, THREAD).await;
    let value_id = body["proposals"][0]["id"].as_i64().unwrap();
    let stage_id = body["proposals"][1]["id"].as_i64().unwrap();

    let (status, accepted) = app
        .post(&format!("/proposals/{value_id}/accept"), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    assert_eq!(accepted["proposal"]["status"], "accepted");
    assert_eq!(accepted["opportunity"]["value_cents"], 1_245_000);

    let (status, rejected) = app
        .post(&format!("/proposals/{stage_id}/reject"), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rejected["status"], "rejected");

    let (_, detail) = app.get(&format!("/opportunities/{id}")).await;
    assert_eq!(detail["opportunity"]["value_cents"], 1_245_000);
    assert_eq!(detail["opportunity"]["stage"], "qualified");
    assert_eq!(detail["pending_proposals"], json!([]));

    // Resolved proposals cannot be resolved again.
    for path in [
        format!("/proposals/{value_id}/accept"),
        format!("/proposals/{value_id}/reject"),
        format!("/proposals/{stage_id}/accept"),
    ] {
        let (status, err) = app.post(&path, json!({})).await;
        assert_eq!(status, StatusCode::CONFLICT, "{path}");
        assert_eq!(err["error"]["code"], "conflict");
    }
    let (status, _) = app.post("/proposals/999/accept", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn proposals_carry_the_evidence_quote_and_a_dismissal_leaves_a_timeline_note() {
    let app = TestApp::new().await;
    let id = app
        .create_opportunity(json!({
            "company": "Rivet Box Co", "value_cents": 1_000_000, "stage": "qualified",
        }))
        .await;
    let body = paste(&app, id, THREAD).await;
    let value_proposal = &body["proposals"][0];
    assert_eq!(value_proposal["field"], "value_cents");
    assert_eq!(
        value_proposal["evidence"],
        "Thanks for the quote. $12,450.00 for 5,000 units works for us."
    );
    let stage_id = body["proposals"][1]["id"].as_i64().unwrap();

    let (status, rejected) = app
        .post(&format!("/proposals/{stage_id}/reject"), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rejected["status"], "rejected");

    let (_, detail) = app.get(&format!("/opportunities/{id}")).await;
    let timeline = detail["timeline"].as_array().unwrap();
    assert!(
        timeline
            .iter()
            .any(|e| e["raw_text"].as_str()
                == Some("Dismissed 1 suggestion from Re: 12x10x8 RSC quote")),
        "{timeline:?}"
    );
}

#[tokio::test]
async fn unreject_and_unaccept_undo_within_the_window() {
    let app = TestApp::new().await;
    let id = app
        .create_opportunity(json!({
            "company": "Rivet Box Co", "value_cents": 1_000_000, "stage": "qualified",
        }))
        .await;
    let body = paste(&app, id, THREAD).await;
    let value_id = body["proposals"][0]["id"].as_i64().unwrap();
    let stage_id = body["proposals"][1]["id"].as_i64().unwrap();

    // Undo a reject.
    app.post(&format!("/proposals/{stage_id}/reject"), json!({}))
        .await;
    let (status, undone) = app
        .post(&format!("/proposals/{stage_id}/unreject"), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{undone}");
    assert_eq!(undone["status"], "proposed");
    // Undoing an already-proposed (not rejected) change is a conflict.
    let (status, _) = app
        .post(&format!("/proposals/{stage_id}/unreject"), json!({}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Undo an accept.
    app.post(&format!("/proposals/{value_id}/accept"), json!({}))
        .await;
    let (status, undone) = app
        .post(&format!("/proposals/{value_id}/unaccept"), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{undone}");
    assert_eq!(undone["status"], "proposed");
    let (_, detail) = app.get(&format!("/opportunities/{id}")).await;
    assert_eq!(detail["opportunity"]["value_cents"], 1_000_000);

    // Re-accept, change the field again directly, then the old undo is a conflict.
    app.post(&format!("/proposals/{value_id}/accept"), json!({}))
        .await;
    app.patch(&format!("/opportunities/{id}"), json!({"value_cents": 999}))
        .await;
    let (status, _) = app
        .post(&format!("/proposals/{value_id}/unaccept"), json!({}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn accept_all_and_dismiss_all() {
    let app = TestApp::new().await;
    let a = app
        .create_opportunity(json!({
            "company": "A", "value_cents": 1, "stage": "lead",
        }))
        .await;
    let b = app
        .create_opportunity(json!({
            "company": "B", "value_cents": 1, "stage": "lead",
        }))
        .await;
    paste(&app, a, THREAD).await;
    paste(&app, b, THREAD).await;

    let (status, body) = app
        .post(
            &format!("/opportunities/{a}/proposals/accept-all"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(fields(&body["accepted"]), vec!["value_cents", "stage"]);
    assert_eq!(body["opportunity"]["value_cents"], 1_245_000);
    assert_eq!(body["opportunity"]["stage"], "proposal");

    let (status, body) = app
        .post(
            &format!("/opportunities/{b}/proposals/dismiss-all"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fields(&body["rejected"]), vec!["value_cents", "stage"]);
    let (_, detail) = app.get(&format!("/opportunities/{b}")).await;
    assert_eq!(detail["opportunity"]["value_cents"], 1);
    assert_eq!(detail["pending_proposals"], json!([]));

    // Nothing pending: both are empty no-ops.
    let (_, body) = app
        .post(
            &format!("/opportunities/{a}/proposals/accept-all"),
            json!({}),
        )
        .await;
    assert_eq!(body["accepted"], json!([]));
    let (status, _) = app
        .post("/opportunities/999/proposals/dismiss-all", json!({}))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn repeated_paste_does_not_duplicate_pending_proposals() {
    let app = TestApp::new().await;
    let id = app
        .create_opportunity(json!({"company": "A", "value_cents": 1, "stage": "lead"}))
        .await;
    paste(&app, id, THREAD).await;
    let second = paste(&app, id, THREAD).await;
    assert_eq!(second["proposals"], json!([]));
    // Fields filled by the first paste now match, so nothing to fill either.
    assert_eq!(second["auto_filled"], json!([]));
    let (_, detail) = app.get(&format!("/opportunities/{id}")).await;
    assert_eq!(detail["pending_proposals"].as_array().unwrap().len(), 2);
    assert_eq!(detail["timeline"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn last_touch_never_moves_backwards_or_into_the_future() {
    let app = TestApp::new().await;
    let recent = days_ago(1);
    let id = app
        .create_opportunity(json!({"company": "A", "last_touch_at": recent}))
        .await;

    // An old thread.
    let body = paste(&app, id, "Spoke with them on 2025-01-10.").await;
    assert_eq!(body["entry"]["occurred_at"], "2025-01-10 00:00:00");
    assert_eq!(fields(&body["auto_filled"]), Vec::<&str>::new());

    // A date in the future is a deadline, not a touch.
    let future = &days_ago(-10)[..10];
    let body = paste(&app, id, &format!("Delivery promised for {future}.")).await;
    assert_eq!(body["entry"]["occurred_at"], json!(null));
    assert_eq!(fields(&body["auto_filled"]), Vec::<&str>::new());

    let (_, detail) = app.get(&format!("/opportunities/{id}")).await;
    assert_eq!(detail["opportunity"]["last_touch_at"], recent);
}

#[tokio::test]
async fn paste_validation() {
    let app = TestApp::new().await;
    let id = app.create_opportunity(json!({"company": "A"})).await;
    let (status, _) = app
        .post(&format!("/opportunities/{id}/paste"), json!({"text": "  "}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = app
        .post("/opportunities/999/paste", json!({"text": "hello"}))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM timeline_entries")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

/// Fails every call, after recording how many timeline entries the database
/// held at the moment it was called.
struct FailingExtractor {
    pool: Mutex<Option<SqlitePool>>,
    seen_entries: Mutex<Option<i64>>,
}

#[async_trait]
impl Extractor for FailingExtractor {
    async fn extract(&self, input: ExtractionInput<'_>) -> Result<Extraction, ExtractError> {
        let pool = self.pool.lock().unwrap().clone().unwrap();
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM timeline_entries WHERE raw_text = ?1")
                .bind(input.text)
                .fetch_one(&pool)
                .await
                .unwrap();
        *self.seen_entries.lock().unwrap() = Some(count);
        Err(ExtractError::Status {
            status: 503,
            body: "model overloaded".to_owned(),
        })
    }
}

#[tokio::test]
async fn entry_is_saved_before_extraction_and_survives_its_failure() {
    let extractor = Arc::new(FailingExtractor {
        pool: Mutex::new(None),
        seen_entries: Mutex::new(None),
    });
    let app = TestApp::with_extractor(extractor.clone()).await;
    *extractor.pool.lock().unwrap() = Some(app.pool.clone());
    let id = app
        .create_opportunity(json!({"company": "A", "value_cents": 1}))
        .await;

    let body = paste(&app, id, THREAD).await;

    // The entry was committed before the extractor ran.
    assert_eq!(*extractor.seen_entries.lock().unwrap(), Some(1));
    assert_eq!(body["entry"]["raw_text"], THREAD);
    assert_eq!(body["entry"]["summary"], json!(null));
    assert_eq!(
        body["extraction_error"],
        "extractor returned HTTP 503: model overloaded"
    );
    assert_eq!(body["proposals"], json!([]));
    assert_eq!(body["auto_filled"], json!([]));
    assert_eq!(body["suggested_tasks"], json!([]));

    let (_, detail) = app.get(&format!("/opportunities/{id}")).await;
    assert_eq!(detail["timeline"][0]["raw_text"], THREAD);
    assert_eq!(detail["opportunity"]["value_cents"], 1);
    assert_eq!(detail["opportunity"]["contact_name"], json!(null));
}

/// Returns whatever it was built with: output a real model might produce.
struct FixedExtractor(Extraction);

#[async_trait]
impl Extractor for FixedExtractor {
    async fn extract(&self, _: ExtractionInput<'_>) -> Result<Extraction, ExtractError> {
        Ok(self.0.clone())
    }
}

#[tokio::test]
async fn invalid_extractor_output_is_dropped() {
    let app = TestApp::with_extractor(Arc::new(FixedExtractor(Extraction {
        stage: Some("negotiating".to_owned()),
        contact_email: Some("not-an-email".to_owned()),
        value_cents: Some(-100),
        latest_date: Some("last tuesday".to_owned()),
        next_step: Some("  ".to_owned()),
        tasks: vec![
            lula_dog_backend::extractor::SuggestedTask {
                title: "  ".to_owned(),
                due_at: None,
            },
            lula_dog_backend::extractor::SuggestedTask {
                title: "Real task".to_owned(),
                due_at: Some("someday".to_owned()),
            },
        ],
        ..Extraction::default()
    })))
    .await;
    let id = app.create_opportunity(json!({"company": "A"})).await;
    let body = paste(&app, id, "anything").await;
    assert_eq!(body["proposals"], json!([]));
    assert_eq!(body["auto_filled"], json!([]));
    assert_eq!(body["extraction_error"], json!(null));
    let tasks = body["suggested_tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["title"], "Real task");
    assert_eq!(tasks[0]["due_at"], json!(null));
}

#[tokio::test]
async fn timeline_is_append_only() {
    let app = TestApp::new().await;
    let id = app.create_opportunity(json!({"company": "A"})).await;
    let body = paste(&app, id, THREAD).await;
    let entry_id = body["entry"]["id"].as_i64().unwrap();

    for sql in [
        "UPDATE timeline_entries SET raw_text = 'edited' WHERE id = ?1",
        "UPDATE timeline_entries SET summary = 'rewritten' WHERE id = ?1",
        "UPDATE timeline_entries SET occurred_at = '2020-01-01 00:00:00' WHERE id = ?1",
        "DELETE FROM timeline_entries WHERE id = ?1",
    ] {
        let err = sqlx::query(sql)
            .bind(entry_id)
            .execute(&app.pool)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("append-only"), "{sql}: {err}");
    }
}
