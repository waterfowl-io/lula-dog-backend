//! The IL SOS LLC fixed-width importer: the master/name/agent join, the
//! cheap filter running against the joined row, idempotent re-import, and
//! the required-file-missing error. Fixed-width lines are built in this
//! test (not checked in as fixtures) so the byte offsets stay obviously
//! correct relative to `src/importers/il_sos_llc.rs`'s field table, which
//! comes from `schemas/*.schema` in github.com/fgregg/il-corporate-filings.

#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a failing assertion aborting the test is the mechanism"
)]

mod common;

use common::TestApp;
use lula_dog_backend::bucket::Bucket;
use lula_dog_backend::importers::il_sos_llc;
use lula_dog_backend::target_profiles::{self, NewTargetProfile};

fn pad(s: &str, width: usize) -> String {
    format!("{s:<width$}")
}

/// A `llcallmst.txt` line: `file_number`(8) `purpose_code`(6) `status_code`(2)
/// `status_date`(8) `organized_date`(8) `dissolution_date`(8)
/// `management_type`(1) `juris_organized`(2) `records_off_street`(45)
/// `records_off_city`(30) `records_off_zip`(9). 127 bytes, matching offsets
/// up to the zip field.
fn master_line(
    file_number: &str,
    status_code: &str,
    organized_date: &str,
    street: &str,
    city: &str,
) -> String {
    format!(
        "{}{}{}{}{}{}{}{}{}{}{}",
        pad(file_number, 8),
        pad("", 6),
        pad(status_code, 2),
        pad("", 8),
        pad(organized_date, 8),
        pad("", 8),
        pad("", 1),
        pad("", 2),
        pad(street, 45),
        pad(city, 30),
        pad("", 9),
    )
}

/// `llcallnam.txt`: `file_number`(8) `name`(120).
fn name_line(file_number: &str, name: &str) -> String {
    format!("{}{}", pad(file_number, 8), pad(name, 120))
}

/// `llcallagt.txt`: `file_number`(8) `agent_code`(1) `agent_name`(60).
fn agent_line(file_number: &str, agent_name: &str) -> String {
    format!(
        "{}{}{}",
        pad(file_number, 8),
        pad("", 1),
        pad(agent_name, 60)
    )
}

async fn jacks_profile(pool: &sqlx::SqlitePool) -> target_profiles::TargetProfile {
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
            include_keywords: vec!["coffee".to_owned()],
            exclude_keywords: vec![],
            min_fit_score: Some(50),
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn joins_master_name_and_agent_and_runs_the_cheap_filter() {
    let app = TestApp::new().await;
    let dir = tempfile::tempdir().unwrap();
    let bucket = Bucket::new(dir.path().join("bucket"));
    let profile = jacks_profile(&app.pool).await;

    let master = dir.path().join("llcallmst.txt");
    std::fs::write(
        &master,
        format!(
            "{}\n{}\n",
            master_line("00112233", "AC", "20260115", "100 MAIN ST", "PEORIA"),
            master_line("00112244", "AC", "20260201", "200 OAK AVE", "CHICAGO"),
        ),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("llcallnam.txt"),
        format!(
            "{}\n{}\n",
            name_line("00112233", "ACME COFFEE ROASTERS LLC"),
            name_line("00112244", "NORTHWEST REGISTERED AGENT LLC"),
        ),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("llcallagt.txt"),
        format!(
            "{}\n{}\n",
            agent_line("00112233", "JORDAN REYES"),
            agent_line("00112244", "NORTHWEST REGISTERED AGENT LLC"),
        ),
    )
    .unwrap();

    let summary = il_sos_llc::import(&app.pool, &bucket, &master, Some(&profile))
        .await
        .unwrap();
    assert_eq!(summary.seen, 2);
    assert_eq!(summary.new, 2);
    assert_eq!(summary.error, 0);
    assert_eq!(summary.filtered, 1, "the self-named registered agent");

    let rows: Vec<(String, Option<String>, Option<String>, String)> = sqlx::query_as(
        "SELECT name, registered_agent, formation_date, filter_status
         FROM companies ORDER BY name",
    )
    .fetch_all(&app.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "ACME COFFEE ROASTERS LLC");
    assert_eq!(rows[0].1.as_deref(), Some("JORDAN REYES"));
    assert_eq!(rows[0].2.as_deref(), Some("2026-01-15 00:00:00"));
    assert_eq!(rows[0].3, "passed");
    assert_eq!(rows[1].0, "NORTHWEST REGISTERED AGENT LLC");
    assert_eq!(rows[1].3, "filtered");

    let pre_score: Option<i64> = sqlx::query_scalar(
        "SELECT pre_score FROM companies WHERE name = 'ACME COFFEE ROASTERS LLC'",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(pre_score, Some(10), "matched the \"coffee\" keyword");
}

#[tokio::test]
async fn works_without_an_agent_file() {
    let app = TestApp::new().await;
    let dir = tempfile::tempdir().unwrap();
    let bucket = Bucket::new(dir.path().join("bucket"));

    let master = dir.path().join("llcallmst.txt");
    std::fs::write(
        &master,
        master_line("00999888", "AC", "20260301", "1 ELM ST", "SPRINGFIELD") + "\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("llcallnam.txt"),
        name_line("00999888", "PLAIN CO LLC") + "\n",
    )
    .unwrap();

    let summary = il_sos_llc::import(&app.pool, &bucket, &master, None)
        .await
        .unwrap();
    assert_eq!(summary.new, 1);
    let agent: Option<String> =
        sqlx::query_scalar("SELECT registered_agent FROM companies WHERE name = 'PLAIN CO LLC'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(agent, None);
}

#[tokio::test]
async fn missing_name_file_is_an_error() {
    let app = TestApp::new().await;
    let dir = tempfile::tempdir().unwrap();
    let bucket = Bucket::new(dir.path().join("bucket"));
    let master = dir.path().join("llcallmst.txt");
    std::fs::write(
        &master,
        master_line("00000001", "AC", "20260101", "", "") + "\n",
    )
    .unwrap();

    let err = il_sos_llc::import(&app.pool, &bucket, &master, None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("llcallnam.txt"), "{err}");
}

#[tokio::test]
async fn reimporting_the_same_master_file_is_idempotent() {
    let app = TestApp::new().await;
    let dir = tempfile::tempdir().unwrap();
    let bucket = Bucket::new(dir.path().join("bucket"));
    let master = dir.path().join("llcallmst.txt");
    std::fs::write(
        &master,
        master_line("00445566", "AC", "20260401", "9 PINE ST", "ROCKFORD") + "\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("llcallnam.txt"),
        name_line("00445566", "GRANITE BAKERY SUPPLY CO") + "\n",
    )
    .unwrap();

    il_sos_llc::import(&app.pool, &bucket, &master, None)
        .await
        .unwrap();
    let second = il_sos_llc::import(&app.pool, &bucket, &master, None)
        .await
        .unwrap();

    assert_eq!(second.new, 0);
    assert_eq!(second.duplicate, 1);
    let companies: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM companies")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(companies, 1);
}
