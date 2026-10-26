//! CORS allowlist (plan B9): `https://app.lula.dog` and
//! `http://localhost:5173` get `Access-Control-Allow-Origin` echoed back
//! and a cached preflight; an unlisted origin gets neither. No credentials
//! or cookies -- auth is a bearer token.

#![expect(
    clippy::unwrap_used,
    reason = "a failing assertion aborting the test is the mechanism"
)]

mod common;

use axum::http::Method;
use common::TestApp;

#[tokio::test]
async fn an_allowed_origin_gets_acao_echoed_back() {
    let app = TestApp::new().await;
    let res = app
        .raw_request(
            Method::GET,
            "/health",
            &[("origin", "https://app.lula.dog")],
        )
        .await;
    assert_eq!(
        res.headers().get("access-control-allow-origin").unwrap(),
        "https://app.lula.dog"
    );
}

#[tokio::test]
async fn the_localhost_dev_origin_is_also_allowed() {
    let app = TestApp::new().await;
    let res = app
        .raw_request(
            Method::GET,
            "/health",
            &[("origin", "http://localhost:5173")],
        )
        .await;
    assert_eq!(
        res.headers().get("access-control-allow-origin").unwrap(),
        "http://localhost:5173"
    );
}

#[tokio::test]
async fn an_unlisted_origin_gets_no_acao() {
    let app = TestApp::new().await;
    let res = app
        .raw_request(
            Method::GET,
            "/health",
            &[("origin", "https://evil.example")],
        )
        .await;
    assert!(res.headers().get("access-control-allow-origin").is_none());
}

#[tokio::test]
async fn a_preflight_from_an_allowed_origin_gets_max_age() {
    let app = TestApp::new().await;
    let res = app
        .raw_request(
            Method::OPTIONS,
            "/opportunities",
            &[
                ("origin", "https://app.lula.dog"),
                ("access-control-request-method", "POST"),
                (
                    "access-control-request-headers",
                    "authorization,content-type",
                ),
            ],
        )
        .await;
    assert_eq!(
        res.headers().get("access-control-allow-origin").unwrap(),
        "https://app.lula.dog"
    );
    assert_eq!(
        res.headers().get("access-control-max-age").unwrap(),
        "86400"
    );
}

/// The browser only honors a preflight when `Access-Control-Allow-Origin`
/// matches, so `Access-Control-Max-Age` being present regardless is
/// harmless -- `Allow-Origin`'s absence is what actually blocks the
/// unlisted origin, and that's the one this test asserts.
#[tokio::test]
async fn a_preflight_from_an_unlisted_origin_gets_no_acao() {
    let app = TestApp::new().await;
    let res = app
        .raw_request(
            Method::OPTIONS,
            "/opportunities",
            &[
                ("origin", "https://evil.example"),
                ("access-control-request-method", "POST"),
            ],
        )
        .await;
    assert!(res.headers().get("access-control-allow-origin").is_none());
}

#[tokio::test]
async fn no_response_ever_carries_allow_credentials() {
    let app = TestApp::new().await;
    let res = app
        .raw_request(
            Method::GET,
            "/health",
            &[("origin", "https://app.lula.dog")],
        )
        .await;
    assert!(
        res.headers()
            .get("access-control-allow-credentials")
            .is_none()
    );
}
