//! The Bizapedia CSV importer: default column mapping, a configured
//! override mapping, the cheap filter running once per company, and
//! idempotent re-import.

#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a failing assertion aborting the test is the mechanism"
)]

mod common;

use common::TestApp;
use lula_dog_backend::bucket::Bucket;
use lula_dog_backend::importers::bizapedia::{self, ColumnMapping};
use lula_dog_backend::target_profiles::{self, NewTargetProfile};
use sqlx::SqlitePool;

const FIXTURE: &str = "tests/fixtures/bizapedia_fake_filings.csv";

async fn jacks_profile(pool: &SqlitePool) -> target_profiles::TargetProfile {
    let mut conn = pool.acquire().await.unwrap();
    target_profiles::insert(
        &mut conn,
        NewTargetProfile {
            name: "test profile".to_owned(),
            industries: vec![],
            regions: vec!["IL".to_owned()],
            size_min: None,
            size_max: None,
            size_notes: None,
            include_keywords: vec!["coffee".to_owned(), "bakery".to_owned()],
            exclude_keywords: vec!["law firm".to_owned()],
            min_fit_score: Some(50),
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn imports_filters_and_scores_with_the_default_mapping() {
    let app = TestApp::new().await;
    let dir = tempfile::tempdir().unwrap();
    let bucket = Bucket::new(dir.path().join("bucket"));
    let profile = jacks_profile(&app.pool).await;

    let summary = bizapedia::import(
        &app.pool,
        &bucket,
        std::path::Path::new(FIXTURE),
        &ColumnMapping::default(),
        Some(&profile),
    )
    .await
    .unwrap();

    assert_eq!(summary.seen, 5);
    assert_eq!(summary.new, 5);
    assert_eq!(summary.duplicate, 0);
    assert_eq!(summary.error, 0);
    // Bluebird Holdings (shell), Riverside Boxes (revoked), Prairie Law Firm
    // (excluded keyword).
    assert_eq!(summary.filtered, 3);

    let rows: Vec<(String, String, Option<i64>)> =
        sqlx::query_as("SELECT name, filter_status, pre_score FROM companies ORDER BY name")
            .fetch_all(&app.pool)
            .await
            .unwrap();
    assert_eq!(rows.len(), 5);
    let by_name: std::collections::HashMap<_, _> =
        rows.into_iter().map(|(n, s, p)| (n, (s, p))).collect();
    assert_eq!(
        by_name["Acme Coffee Roasters LLC"],
        ("passed".to_owned(), Some(10))
    );
    assert_eq!(by_name["Bluebird Holdings LLC"].0, "filtered".to_owned());
    assert_eq!(by_name["Riverside Boxes Inc"].0, "filtered".to_owned());
    assert_eq!(by_name["Prairie Law Firm LLC"].0, "filtered".to_owned());
    assert_eq!(
        by_name["Granite Bakery Supply Co"],
        ("passed".to_owned(), Some(10))
    );

    // The raw file was archived unchanged.
    let archived = dir
        .path()
        .join("bucket")
        .join("sources")
        .join("bizapedia")
        .join(chrono::Utc::now().format("%Y-%m-%d").to_string())
        .join("bizapedia_fake_filings.csv");
    assert!(archived.exists(), "{}", archived.display());
    assert_eq!(
        std::fs::read_to_string(archived).unwrap(),
        std::fs::read_to_string(FIXTURE).unwrap()
    );

    let coverage: (String, Option<String>, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT source, period, seen_count, new_count, duplicate_count, filtered_count
         FROM coverage_log",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(coverage.0, "bizapedia");
    assert_eq!(coverage.1.as_deref(), Some("bizapedia_fake_filings.csv"));
    assert_eq!(coverage.2, 5);
    assert_eq!(coverage.3, 5);
    assert_eq!(coverage.4, 0);
    assert_eq!(coverage.5, 3);

    let source_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM company_sources")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(source_count, 5);
}

#[tokio::test]
async fn reimporting_the_same_file_is_idempotent() {
    let app = TestApp::new().await;
    let dir = tempfile::tempdir().unwrap();
    let bucket = Bucket::new(dir.path().join("bucket"));
    let profile = jacks_profile(&app.pool).await;
    let path = std::path::Path::new(FIXTURE);

    bizapedia::import(
        &app.pool,
        &bucket,
        path,
        &ColumnMapping::default(),
        Some(&profile),
    )
    .await
    .unwrap();
    let second = bizapedia::import(
        &app.pool,
        &bucket,
        path,
        &ColumnMapping::default(),
        Some(&profile),
    )
    .await
    .unwrap();

    assert_eq!(second.seen, 5);
    assert_eq!(second.new, 0);
    assert_eq!(second.duplicate, 5);

    let companies: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM companies")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(companies, 5, "no duplicate companies");
    let sources: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM company_sources")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(sources, 5, "no duplicate source rows");
    let coverage_runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM coverage_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(coverage_runs, 2, "one coverage row per run, even when idle");
}

#[tokio::test]
async fn a_mapping_file_overrides_header_aliases_case_and_whitespace_insensitively() {
    let app = TestApp::new().await;
    let dir = tempfile::tempdir().unwrap();
    let bucket = Bucket::new(dir.path().join("bucket"));

    let csv_path = dir.path().join("odd_headers.csv");
    std::fs::write(
        &csv_path,
        "Company   Name,State File #,Filed On\n\
         Acme Widgets LLC,99887766,2026-08-01\n",
    )
    .unwrap();
    let mapping_path = dir.path().join("mapping.toml");
    std::fs::write(
        &mapping_path,
        "entity_name = [\"company name\"]\n\
         file_number = [\"state file #\"]\n\
         filing_date = [\"filed on\"]\n",
    )
    .unwrap();

    let mapping = ColumnMapping::load(Some(&mapping_path)).unwrap();
    let summary = bizapedia::import(&app.pool, &bucket, &csv_path, &mapping, None)
        .await
        .unwrap();

    assert_eq!(summary.new, 1);
    let (name, file_number): (String, Option<String>) =
        sqlx::query_as("SELECT name, state_file_number FROM companies")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(name, "Acme Widgets LLC");
    assert_eq!(file_number.as_deref(), Some("99887766"));
}

#[tokio::test]
async fn rows_without_a_name_are_counted_as_errors_not_failures() {
    let app = TestApp::new().await;
    let dir = tempfile::tempdir().unwrap();
    let bucket = Bucket::new(dir.path().join("bucket"));
    let csv_path = dir.path().join("missing_name.csv");
    std::fs::write(
        &csv_path,
        "Entity Name,File Number\n\
         ,11112222\n\
         Acme Good Co,33334444\n",
    )
    .unwrap();

    let summary = bizapedia::import(
        &app.pool,
        &bucket,
        &csv_path,
        &ColumnMapping::default(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(summary.seen, 2);
    assert_eq!(summary.error, 1);
    assert_eq!(summary.new, 1);
}
