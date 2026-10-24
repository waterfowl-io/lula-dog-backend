//! `run_enrich` (places + crawl) and `run_classify`, with stub backends:
//! skip-already-done selection, filtered companies never enriched, and
//! queueing a lead candidate above the profile's threshold.

#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a failing assertion aborting the test is the mechanism"
)]

mod common;

use async_trait::async_trait;
use common::TestApp;
use lula_dog_backend::bucket::Bucket;
use lula_dog_backend::companies::{self, FilterOutcome, FilterStatus, NewCompany};
use lula_dog_backend::config::CrawlLimits;
use lula_dog_backend::crawler::StubFetcher;
use lula_dog_backend::enrichment;
use lula_dog_backend::places::{PlaceQuery, PlacesClient, PlacesError, StubPlacesClient};
use lula_dog_backend::target_profiles::{self, NewTargetProfile, TargetProfile};

fn limits() -> CrawlLimits {
    CrawlLimits {
        max_pages: 4,
        timeout_secs: 5,
        max_bytes: 1_000_000,
    }
}

async fn passing_profile(pool: &sqlx::SqlitePool) -> TargetProfile {
    let mut conn = pool.acquire().await.unwrap();
    target_profiles::insert(
        &mut conn,
        NewTargetProfile {
            name: "test".to_owned(),
            industries: vec![],
            regions: vec!["IL".to_owned()],
            size_min: None,
            size_max: None,
            size_notes: None,
            include_keywords: vec!["coffee".to_owned()],
            exclude_keywords: vec![],
            min_fit_score: Some(10),
        },
    )
    .await
    .unwrap()
}

async fn passed_company(pool: &sqlx::SqlitePool, name: &str) -> i64 {
    let mut conn = pool.acquire().await.unwrap();
    let company = companies::upsert(
        &mut conn,
        NewCompany {
            name: name.to_owned(),
            jurisdiction: "IL".to_owned(),
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
    company.id
}

#[tokio::test]
async fn enrich_then_classify_queues_a_lead() {
    let app = TestApp::new().await;
    let dir = tempfile::tempdir().unwrap();
    let bucket = Bucket::new(dir.path().join("bucket"));
    let profile = passing_profile(&app.pool).await;
    let company_id = passed_company(&app.pool, "Acme Coffee Roasters").await;

    let enrich_summary = enrichment::run_enrich(
        &app.pool,
        &bucket,
        &StubPlacesClient,
        &StubFetcher,
        limits(),
        10,
    )
    .await
    .unwrap();
    assert_eq!(enrich_summary.places.attempted, 1);
    assert_eq!(enrich_summary.places.success, 1);
    assert_eq!(enrich_summary.crawl.attempted, 1);
    assert_eq!(enrich_summary.crawl.success, 1);

    let company = {
        let mut conn = app.pool.acquire().await.unwrap();
        companies::fetch(&mut conn, company_id).await.unwrap()
    };
    assert_eq!(
        company.website.as_deref(),
        Some("https://acme-coffee-roasters.example.com")
    );
    assert_eq!(
        company.domain.as_deref(),
        Some("acme-coffee-roasters.example.com")
    );

    let kinds: Vec<String> = sqlx::query_scalar(
        "SELECT kind FROM company_enrichments WHERE company_id = ?1 ORDER BY kind",
    )
    .bind(company_id)
    .fetch_all(&app.pool)
    .await
    .unwrap();
    assert_eq!(kinds, vec!["crawl".to_owned(), "places".to_owned()]);

    let crawl_files: String = sqlx::query_scalar(
        "SELECT files FROM company_enrichments WHERE company_id = ?1 AND kind = 'crawl'",
    )
    .bind(company_id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    let files: Vec<String> = serde_json::from_str(&crawl_files).unwrap();
    assert_ne!(files, Vec::<String>::new());
    assert!(dir.path().join("bucket").join(&files[0]).exists());

    let classify_summary = enrichment::run_classify(
        &app.pool,
        &lula_dog_backend::classifier::StubClassifier,
        &profile,
        10,
    )
    .await
    .unwrap();
    assert_eq!(classify_summary.attempted, 1);
    assert_eq!(classify_summary.success, 1);

    let (status, got) = app.get("/leads").await;
    assert_eq!(status, axum::http::StatusCode::OK, "{got}");
    let leads = got.as_array().unwrap();
    assert_eq!(leads.len(), 1, "{got}");
    assert_eq!(leads[0]["company"]["name"], "Acme Coffee Roasters");
    assert_eq!(leads[0]["fit_score"], 10);
    assert_eq!(leads[0]["classification"]["industry"], "coffee");

    // A second run processes nothing new: each company_enrichments kind
    // already has a row.
    let again = enrichment::run_enrich(
        &app.pool,
        &bucket,
        &StubPlacesClient,
        &StubFetcher,
        limits(),
        10,
    )
    .await
    .unwrap();
    assert_eq!(again.places.attempted, 0);
    assert_eq!(again.crawl.attempted, 0);
    let classify_again = enrichment::run_classify(
        &app.pool,
        &lula_dog_backend::classifier::StubClassifier,
        &profile,
        10,
    )
    .await
    .unwrap();
    assert_eq!(classify_again.attempted, 0);
}

#[tokio::test]
async fn filtered_companies_are_never_enriched() {
    let app = TestApp::new().await;
    let dir = tempfile::tempdir().unwrap();
    let bucket = Bucket::new(dir.path().join("bucket"));
    let mut conn = app.pool.acquire().await.unwrap();
    let filtered = companies::upsert(
        &mut conn,
        NewCompany {
            name: "Acme Holdings LLC".to_owned(),
            jurisdiction: "IL".to_owned(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    companies::set_filter_outcome(
        &mut conn,
        filtered.id,
        &FilterOutcome {
            status: FilterStatus::Filtered,
            reason: Some("likely_shell_or_holding_company".to_owned()),
            pre_score: None,
        },
    )
    .await
    .unwrap();
    drop(conn);

    let summary = enrichment::run_enrich(
        &app.pool,
        &bucket,
        &StubPlacesClient,
        &StubFetcher,
        limits(),
        10,
    )
    .await
    .unwrap();
    assert_eq!(summary.places.attempted, 0);
    assert_eq!(summary.crawl.attempted, 0);
}

struct AlwaysErrorsPlaces;

#[async_trait]
impl PlacesClient for AlwaysErrorsPlaces {
    async fn search(
        &self,
        _query: PlaceQuery<'_>,
    ) -> Result<Option<lula_dog_backend::places::PlaceResult>, PlacesError> {
        Err(PlacesError::Request("boom".to_owned()))
    }
}

#[tokio::test]
async fn a_places_error_is_recorded_and_crawl_is_skipped_without_a_website() {
    let app = TestApp::new().await;
    let dir = tempfile::tempdir().unwrap();
    let bucket = Bucket::new(dir.path().join("bucket"));
    let company_id = passed_company(&app.pool, "Acme Mystery Co").await;

    let summary = enrichment::run_enrich(
        &app.pool,
        &bucket,
        &AlwaysErrorsPlaces,
        &StubFetcher,
        limits(),
        10,
    )
    .await
    .unwrap();
    assert_eq!(summary.places.attempted, 1);
    assert_eq!(summary.places.error, 1);
    assert_eq!(summary.crawl.attempted, 0, "no website to crawl");

    let (status, error): (String, Option<String>) = sqlx::query_as(
        "SELECT status, error FROM company_enrichments WHERE company_id = ?1 AND kind = 'places'",
    )
    .bind(company_id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(status, "error");
    assert!(error.unwrap().contains("boom"));
}
