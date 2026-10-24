//! `companies::upsert` dedupe/merge: by domain, by `(jurisdiction,
//! state_file_number)`, and a domain discovered later re-keying onto the
//! file-number-identified row instead of duplicating it.

#![expect(
    clippy::unwrap_used,
    reason = "a failing assertion aborting the test is the mechanism"
)]

mod common;

use common::TestApp;
use lula_dog_backend::companies::{self, NewCompany};

#[tokio::test]
async fn merges_by_domain_when_two_sources_report_the_same_website() {
    let app = TestApp::new().await;
    let mut conn = app.pool.acquire().await.unwrap();

    let first = companies::upsert(
        &mut conn,
        NewCompany {
            name: "Acme Boxes".to_owned(),
            jurisdiction: "IL".to_owned(),
            website: Some("https://acme.example".to_owned()),
            registered_agent: Some("Jordan Reyes".to_owned()),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let second = companies::upsert(
        &mut conn,
        NewCompany {
            name: "Acme Boxes LLC".to_owned(),
            jurisdiction: "IL".to_owned(),
            website: Some("https://www.acme.example/home".to_owned()),
            state_file_number: Some("00112233".to_owned()),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(first.id, second.id, "same domain, must be one row");
    assert_eq!(second.name, "Acme Boxes", "the first-seen name is kept");
    assert_eq!(second.state_file_number.as_deref(), Some("00112233"));
    assert_eq!(second.registered_agent.as_deref(), Some("Jordan Reyes"));

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM companies")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn merges_by_file_number_when_no_domain_is_known_yet() {
    let app = TestApp::new().await;
    let mut conn = app.pool.acquire().await.unwrap();

    let first = companies::upsert(
        &mut conn,
        NewCompany {
            name: "Bluebird Apparel LLC".to_owned(),
            jurisdiction: "IL".to_owned(),
            state_file_number: Some("00998877".to_owned()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(first.domain, None);

    let second = companies::upsert(
        &mut conn,
        NewCompany {
            name: "Bluebird Apparel".to_owned(),
            jurisdiction: "IL".to_owned(),
            state_file_number: Some("00998877".to_owned()),
            website: Some("https://bluebird-apparel.example".to_owned()),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(first.id, second.id);
    assert_eq!(second.domain.as_deref(), Some("bluebird-apparel.example"));

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM companies")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        count, 1,
        "the domain discovered later must not duplicate the row"
    );
}

#[tokio::test]
async fn different_domains_or_file_numbers_are_different_companies() {
    let app = TestApp::new().await;
    let mut conn = app.pool.acquire().await.unwrap();

    companies::upsert(
        &mut conn,
        NewCompany {
            name: "Acme A".to_owned(),
            jurisdiction: "IL".to_owned(),
            website: Some("https://a.example".to_owned()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    companies::upsert(
        &mut conn,
        NewCompany {
            name: "Acme B".to_owned(),
            jurisdiction: "IL".to_owned(),
            website: Some("https://b.example".to_owned()),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM companies")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(count, 2);
}
