//! GET /leads (filters, sort) and accept/reject/snooze.

#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a failing assertion aborting the test is the mechanism"
)]

mod common;

use axum::http::StatusCode;
use common::{TestApp, days_ago};
use lula_dog_backend::companies::{self, FilterOutcome, FilterStatus, NewCompany};
use lula_dog_backend::leads;
use lula_dog_backend::target_profiles::{self, NewTargetProfile};
use serde_json::json;

async fn profile_id(pool: &sqlx::SqlitePool) -> i64 {
    let mut conn = pool.acquire().await.unwrap();
    target_profiles::insert(
        &mut conn,
        NewTargetProfile {
            name: "test".to_owned(),
            industries: vec![],
            regions: vec![],
            size_min: None,
            size_max: None,
            size_notes: None,
            include_keywords: vec![],
            exclude_keywords: vec![],
            min_fit_score: Some(50),
        },
    )
    .await
    .unwrap()
    .id
}

async fn queued_company(
    pool: &sqlx::SqlitePool,
    name: &str,
    profile_id: i64,
    fit_score: i64,
) -> i64 {
    let mut conn = pool.acquire().await.unwrap();
    let company = companies::upsert(
        &mut conn,
        NewCompany {
            name: name.to_owned(),
            jurisdiction: "IL".to_owned(),
            website: Some(format!(
                "https://{}.example.com",
                name.to_lowercase().replace(' ', "-")
            )),
            phone: Some("555-0100".to_owned()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    companies::set_filter_outcome(
        &mut conn,
        company.id,
        &FilterOutcome {
            status: FilterStatus::Passed,
            reason: None,
            pre_score: Some(0),
        },
    )
    .await
    .unwrap();
    let candidate = leads::queue_candidate(
        &mut conn,
        company.id,
        profile_id,
        fit_score,
        Some("looks good"),
    )
    .await
    .unwrap()
    .unwrap();
    candidate.id
}

#[tokio::test]
async fn lists_new_leads_sorted_by_fit_score_descending() {
    let app = TestApp::new().await;
    let profile = profile_id(&app.pool).await;
    queued_company(&app.pool, "Low Fit Co", profile, 55).await;
    queued_company(&app.pool, "High Fit Co", profile, 90).await;

    let (status, body) = app.get("/leads").await;
    assert_eq!(status, StatusCode::OK);
    let leads = body.as_array().unwrap();
    assert_eq!(leads.len(), 2);
    assert_eq!(leads[0]["company"]["name"], "High Fit Co");
    assert_eq!(leads[1]["company"]["name"], "Low Fit Co");
}

#[tokio::test]
async fn min_score_filter_excludes_lower_scores() {
    let app = TestApp::new().await;
    let profile = profile_id(&app.pool).await;
    queued_company(&app.pool, "Low Fit Co", profile, 55).await;
    queued_company(&app.pool, "High Fit Co", profile, 90).await;

    let (status, body) = app.get("/leads?min_score=80").await;
    assert_eq!(status, StatusCode::OK);
    let leads = body.as_array().unwrap();
    assert_eq!(leads.len(), 1);
    assert_eq!(leads[0]["company"]["name"], "High Fit Co");
}

#[tokio::test]
async fn accept_creates_an_opportunity_with_a_timeline_entry_and_links_it() {
    let app = TestApp::new().await;
    let profile = profile_id(&app.pool).await;
    let lead_id = queued_company(&app.pool, "Rivet Box Co", profile, 70).await;

    let (status, body) = app
        .post(&format!("/leads/{lead_id}/accept"), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let opportunity_id = body["opportunity_id"].as_i64().unwrap();

    let (status, opp) = app.get(&format!("/opportunities/{opportunity_id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(opp["opportunity"]["company"], "Rivet Box Co");
    assert_eq!(opp["opportunity"]["stage"], "lead");
    let timeline = opp["timeline"].as_array().unwrap();
    assert_eq!(timeline.len(), 1);
    let note = timeline[0]["raw_text"].as_str().unwrap();
    assert!(note.contains("Accepted from lead gen"), "{note}");
    assert!(note.contains("rivet-box-co.example.com"), "{note}");
    assert!(note.contains("555-0100"), "{note}");

    let candidate_opportunity: Option<i64> =
        sqlx::query_scalar("SELECT opportunity_id FROM lead_candidates WHERE id = ?1")
            .bind(lead_id)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(candidate_opportunity, Some(opportunity_id));

    // Already decided: a second accept is a 409, not a second opportunity.
    let (status, _) = app
        .post(&format!("/leads/{lead_id}/accept"), json!({}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM opportunities")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn reject_requires_a_reason_and_is_not_listed_as_new() {
    let app = TestApp::new().await;
    let profile = profile_id(&app.pool).await;
    let lead_id = queued_company(&app.pool, "Coastal Ceramics", profile, 60).await;

    let (status, _) = app
        .post(&format!("/leads/{lead_id}/reject"), json!({"reason": ""}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, body) = app
        .post(
            &format!("/leads/{lead_id}/reject"),
            json!({"reason": "wrong industry"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "rejected");
    assert_eq!(body["reject_reason"], "wrong industry");

    let (_, leads) = app.get("/leads").await;
    assert_eq!(leads.as_array().unwrap().len(), 0);

    let (status, _) = app
        .post(
            &format!("/leads/{lead_id}/reject"),
            json!({"reason": "again"}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn snooze_hides_a_lead_until_it_reappears_as_new() {
    let app = TestApp::new().await;
    let profile = profile_id(&app.pool).await;
    let lead_id = queued_company(&app.pool, "Granite State Brewing", profile, 65).await;

    let future = lula_dog_backend::time::format(&(chrono::Utc::now() + chrono::Duration::days(7)));
    let (status, body) = app
        .post(
            &format!("/leads/{lead_id}/snooze"),
            json!({"until": future}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "snoozed");

    let (_, leads) = app.get("/leads").await;
    assert_eq!(leads.as_array().unwrap().len(), 0, "still snoozed");

    // Move the snooze into the past directly (no "snooze until yesterday"
    // API; this is what "time passing" looks like).
    sqlx::query("UPDATE lead_candidates SET snoozed_until = ?1 WHERE id = ?2")
        .bind(days_ago(1))
        .bind(lead_id)
        .execute(&app.pool)
        .await
        .unwrap();

    let (_, leads) = app.get("/leads").await;
    let leads = leads.as_array().unwrap();
    assert_eq!(leads.len(), 1, "expired snooze reappears as new");
    assert_eq!(leads[0]["status"], "new");

    // Now that it is effectively new again, it can be accepted.
    let (status, _) = app
        .post(&format!("/leads/{lead_id}/accept"), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn unknown_lead_is_404() {
    let app = TestApp::new().await;
    let (status, _) = app.post("/leads/999/accept", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = app.post("/leads/999/reject", json!({"reason": "x"})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
