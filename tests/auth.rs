//! JWT verification (HS256, and ES256/RS256 against a loopback JWKS
//! server), the `auth.mode = "dev"` bypass, and the `aal2` gate (every
//! non-public route except `GET /me` -- plan B6). No real network for
//! JWKS: the "server" is a plain `TcpListener` on `127.0.0.1:0` serving a
//! canned response.

#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a failing assertion aborting the test is the mechanism"
)]

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use common::TestApp;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use lula_dog_backend::config::{AuthConfig, AuthMode};
use p256::ecdsa::SigningKey;
use p256::elliptic_curve::pkcs8::EncodePrivateKey;
use p256::pkcs8::LineEnding;
use rsa::RsaPrivateKey;
use rsa::traits::PublicKeyParts;
use serde::Serialize;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;
use utoipa::OpenApi as _;
use uuid::Uuid;

const ISS: &str = "http://127.0.0.1:9/auth/v1";
const AUD: &str = "authenticated";

#[derive(Serialize)]
struct Claims<'a> {
    sub: &'a str,
    email: &'a str,
    aud: &'a str,
    iss: &'a str,
    exp: usize,
    aal: Option<&'a str>,
}

/// `aal` defaults to `"aal2"`: every positive-path test in this file
/// exercises token *validity*, not the assurance-level gate, which has its
/// own dedicated tests below.
fn claims(sub: &str, email: &str, iss: &str, aud: &str, exp_in_future: bool) -> String {
    claims_with_aal(sub, email, iss, aud, exp_in_future, Some("aal2"))
}

fn claims_with_aal(
    sub: &str,
    email: &str,
    iss: &str,
    aud: &str,
    exp_in_future: bool,
    aal: Option<&str>,
) -> String {
    let exp = if exp_in_future {
        usize::try_from(chrono::Utc::now().timestamp()).unwrap() + 3600
    } else {
        1
    };
    serde_json::to_string(&Claims {
        sub,
        email,
        aud,
        iss,
        exp,
        aal,
    })
    .unwrap()
}

fn sign_hs256(secret: &str, sub: &str, iss: &str, aud: &str) -> String {
    sign_hs256_with_aal(secret, sub, iss, aud, Some("aal2"))
}

fn sign_hs256_with_aal(secret: &str, sub: &str, iss: &str, aud: &str, aal: Option<&str>) -> String {
    let header = Header::new(Algorithm::HS256);
    let payload: serde_json::Value =
        serde_json::from_str(&claims_with_aal(sub, "u@test.example", iss, aud, true, aal)).unwrap();
    jsonwebtoken::encode(
        &header,
        &payload,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .unwrap()
}

/// Serves `body` for every connection it accepts, once. Returns the base
/// URL (`http://127.0.0.1:PORT`).
async fn spawn_http_server(body: String) -> String {
    let (base_url, _hits) = spawn_counting_http_server(body).await;
    base_url
}

/// Like [`spawn_http_server`], but also returns a counter of how many
/// connections it has accepted -- so a test can assert a refetch did (or,
/// for the cooldown test below, did NOT) happen.
async fn spawn_counting_http_server(body: String) -> (String, std::sync::Arc<AtomicUsize>) {
    let hits = std::sync::Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits_clone = hits.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let body = body.clone();
            let hits = hits_clone.clone();
            tokio::spawn(async move {
                hits.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0_u8; 4096];
                let _ = socket.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    (format!("http://{addr}"), hits)
}

fn hs256_config(secret: Option<&str>) -> AuthConfig {
    AuthConfig {
        mode: AuthMode::Hs256,
        jwt_secret: secret.map(ToOwned::to_owned),
        issuer: ISS.to_owned(),
        audience: AUD.to_owned(),
        ..AuthConfig::default()
    }
}

/// `jwks` mode with no HS256 secret: an HS256 token must be rejected
/// outright. `jwks_url` is set but never dialed by an HS256-only test.
fn jwks_config(jwks_url: &str, iss: &str) -> AuthConfig {
    AuthConfig {
        mode: AuthMode::Jwks,
        jwks_url: Some(jwks_url.to_owned()),
        issuer: iss.to_owned(),
        audience: AUD.to_owned(),
        ..AuthConfig::default()
    }
}

#[tokio::test]
async fn dev_mode_uses_the_default_user_with_no_header() {
    let app = TestApp::new().await;
    let (status, _) = app.get("/opportunities").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn dev_mode_x_dev_user_header_overrides_the_default() {
    let app = TestApp::new().await;
    let other = Uuid::new_v4();
    let (status, _) = app
        .request_as(other, Method::GET, "/opportunities", None)
        .await;
    // No control plane configured: every workspace id (and every dev user)
    // is accessible as owner, so this still succeeds -- the point is that
    // the header was accepted at all (a bad header value is a 400, tested
    // below).
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn dev_mode_rejects_a_malformed_x_dev_user_header() {
    let app = TestApp::new().await;
    let (status, body) = app
        .request_with_headers(
            Method::GET,
            "/opportunities",
            None,
            &[("x-dev-user", "not-a-uuid")],
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

/// `auth.mode = "dev"` is always treated as `aal2`: there is no second
/// factor to challenge against a bypass that already skips the first.
#[tokio::test]
async fn dev_mode_is_always_aal2() {
    let app = TestApp::new().await;
    let (status, body) = app
        .post("/opportunities", json!({"company": "Dev Co"}))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

#[tokio::test]
async fn oidc_mode_rejects_missing_authorization_header() {
    let app = TestApp::with_auth_mode(jwks_config("http://127.0.0.1:1/unused", ISS)).await;
    let (status, body) = app.get("/opportunities").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

#[tokio::test]
async fn oidc_mode_rejects_a_non_bearer_header() {
    let app = TestApp::with_auth_mode(hs256_config(Some("secret"))).await;
    let (status, body) = app
        .request_with_headers(
            Method::GET,
            "/opportunities",
            None,
            &[("authorization", "Basic abc")],
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

#[tokio::test]
async fn hs256_valid_token_is_accepted() {
    let secret = "test-shared-secret";
    let app = TestApp::with_auth_mode(hs256_config(Some(secret))).await;
    let token = sign_hs256(secret, &Uuid::new_v4().to_string(), ISS, AUD);
    let (status, body) = app
        .request_with_bearer(&token, Method::GET, "/opportunities", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn hs256_wrong_secret_is_rejected() {
    let app = TestApp::with_auth_mode(hs256_config(Some("right-secret"))).await;
    let token = sign_hs256("wrong-secret", &Uuid::new_v4().to_string(), ISS, AUD);
    let (status, body) = app
        .request_with_bearer(&token, Method::GET, "/opportunities", None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

#[tokio::test]
async fn hs256_wrong_audience_is_rejected() {
    let secret = "s";
    let app = TestApp::with_auth_mode(hs256_config(Some(secret))).await;
    let token = sign_hs256(secret, &Uuid::new_v4().to_string(), ISS, "some-other-aud");
    let (status, body) = app
        .request_with_bearer(&token, Method::GET, "/opportunities", None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

#[tokio::test]
async fn hs256_wrong_issuer_is_rejected() {
    let secret = "s";
    let app = TestApp::with_auth_mode(hs256_config(Some(secret))).await;
    let token = sign_hs256(
        secret,
        &Uuid::new_v4().to_string(),
        "http://not-the-issuer/auth/v1",
        AUD,
    );
    let (status, body) = app
        .request_with_bearer(&token, Method::GET, "/opportunities", None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

#[tokio::test]
async fn hs256_expired_token_is_rejected() {
    let secret = "s";
    let app = TestApp::with_auth_mode(hs256_config(Some(secret))).await;
    let header = Header::new(Algorithm::HS256);
    let payload: serde_json::Value = serde_json::from_str(&claims(
        &Uuid::new_v4().to_string(),
        "u@test.example",
        ISS,
        AUD,
        false,
    ))
    .unwrap();
    let token = jsonwebtoken::encode(
        &header,
        &payload,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .unwrap();
    let (status, body) = app
        .request_with_bearer(&token, Method::GET, "/opportunities", None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

#[tokio::test]
async fn hs256_non_uuid_subject_is_rejected() {
    let secret = "s";
    let app = TestApp::with_auth_mode(hs256_config(Some(secret))).await;
    let token = sign_hs256(secret, "not-a-uuid", ISS, AUD);
    let (status, body) = app
        .request_with_bearer(&token, Method::GET, "/opportunities", None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

#[tokio::test]
async fn hs256_without_a_configured_secret_is_rejected() {
    let secret = "s";
    let app = TestApp::with_auth_mode(jwks_config("http://127.0.0.1:1/unused", ISS)).await;
    let token = sign_hs256(secret, &Uuid::new_v4().to_string(), ISS, AUD);
    let (status, body) = app
        .request_with_bearer(&token, Method::GET, "/opportunities", None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

#[tokio::test]
async fn es256_token_verified_against_a_loopback_jwks_server() {
    let signing_key = SigningKey::random(&mut rand::rngs::OsRng);
    let pkcs8 = signing_key.to_pkcs8_pem(LineEnding::LF).unwrap();
    let point = signing_key.verifying_key().to_encoded_point(false);
    let jwks = json!({
        "keys": [{
            "kty": "EC",
            "crv": "P-256",
            "kid": "test-es256",
            "x": URL_SAFE_NO_PAD.encode(point.x().unwrap()),
            "y": URL_SAFE_NO_PAD.encode(point.y().unwrap()),
        }]
    });
    let base_url = spawn_http_server(jwks.to_string()).await;
    let iss = format!("{base_url}/auth/v1");
    let app = TestApp::with_auth_mode(jwks_config(
        &format!("{base_url}/auth/v1/.well-known/jwks.json"),
        &iss,
    ))
    .await;

    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some("test-es256".to_owned());
    let payload: serde_json::Value = serde_json::from_str(&claims(
        &Uuid::new_v4().to_string(),
        "u@test.example",
        &iss,
        AUD,
        true,
    ))
    .unwrap();
    let key = EncodingKey::from_ec_pem(pkcs8.as_bytes()).unwrap();
    let token = jsonwebtoken::encode(&header, &payload, &key).unwrap();

    let (status, body) = app
        .request_with_bearer(&token, Method::GET, "/opportunities", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn rs256_token_verified_against_a_loopback_jwks_server() {
    let private_key = RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
    let public_key = rsa::RsaPublicKey::from(&private_key);
    let pkcs8 = private_key
        .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
        .unwrap();
    let jwks = json!({
        "keys": [{
            "kty": "RSA",
            "kid": "test-rs256",
            "n": URL_SAFE_NO_PAD.encode(public_key.n().to_bytes_be()),
            "e": URL_SAFE_NO_PAD.encode(public_key.e().to_bytes_be()),
        }]
    });
    let base_url = spawn_http_server(jwks.to_string()).await;
    let iss = format!("{base_url}/auth/v1");
    let app = TestApp::with_auth_mode(jwks_config(
        &format!("{base_url}/auth/v1/.well-known/jwks.json"),
        &iss,
    ))
    .await;

    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("test-rs256".to_owned());
    let payload: serde_json::Value = serde_json::from_str(&claims(
        &Uuid::new_v4().to_string(),
        "u@test.example",
        &iss,
        AUD,
        true,
    ))
    .unwrap();
    let key = EncodingKey::from_rsa_pem(pkcs8.as_bytes()).unwrap();
    let token = jsonwebtoken::encode(&header, &payload, &key).unwrap();

    let (status, body) = app
        .request_with_bearer(&token, Method::GET, "/opportunities", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn jwks_without_the_tokens_kid_is_rejected() {
    let jwks = json!({"keys": []});
    let base_url = spawn_http_server(jwks.to_string()).await;
    let iss = format!("{base_url}/auth/v1");
    let app = TestApp::with_auth_mode(jwks_config(
        &format!("{base_url}/auth/v1/.well-known/jwks.json"),
        &iss,
    ))
    .await;

    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some("missing".to_owned());
    let signing_key = SigningKey::random(&mut rand::rngs::OsRng);
    let pkcs8 = signing_key.to_pkcs8_pem(LineEnding::LF).unwrap();
    let payload: serde_json::Value = serde_json::from_str(&claims(
        &Uuid::new_v4().to_string(),
        "u@test.example",
        &iss,
        AUD,
        true,
    ))
    .unwrap();
    let key = EncodingKey::from_ec_pem(pkcs8.as_bytes()).unwrap();
    let token = jsonwebtoken::encode(&header, &payload, &key).unwrap();

    let (status, body) = app
        .request_with_bearer(&token, Method::GET, "/opportunities", None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

/// Finding 3 (prompt-backend-review.md): without a cooldown, every token
/// bearing a `kid` this service hasn't cached makes it refetch the whole
/// JWKS document -- so anyone sending garbage `kid`s can make the API
/// hammer the identity provider. A second unknown `kid` arriving right
/// after the first must not trigger a second network call.
#[tokio::test]
async fn unknown_kid_refetch_is_rate_limited() {
    let jwks = json!({"keys": []});
    let (base_url, hits) = spawn_counting_http_server(jwks.to_string()).await;
    let iss = format!("{base_url}/auth/v1");
    let app = TestApp::with_auth_mode(jwks_config(
        &format!("{base_url}/auth/v1/.well-known/jwks.json"),
        &iss,
    ))
    .await;

    let mint = |kid: &str| {
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(kid.to_owned());
        let signing_key = SigningKey::random(&mut rand::rngs::OsRng);
        let pkcs8 = signing_key.to_pkcs8_pem(LineEnding::LF).unwrap();
        let payload: serde_json::Value = serde_json::from_str(&claims(
            &Uuid::new_v4().to_string(),
            "u@test.example",
            &iss,
            AUD,
            true,
        ))
        .unwrap();
        let key = EncodingKey::from_ec_pem(pkcs8.as_bytes()).unwrap();
        jsonwebtoken::encode(&header, &payload, &key).unwrap()
    };

    let (status, body) = app
        .request_with_bearer(&mint("first-unknown"), Method::GET, "/opportunities", None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    let (status, body) = app
        .request_with_bearer(&mint("second-unknown"), Method::GET, "/opportunities", None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "a second unknown kid within the cooldown window must not refetch the JWKS"
    );
}

// ── 401 bodies stay coarse (review finding 2) ───────────────────

/// `AppError::Unauthorized`'s detail (validation-failure internals like
/// `ExpiredSignature` or `missing kid`) must never reach the client -- it
/// is an oracle for a token forger. Ported from Desde0's coarse
/// `AuthError` bodies (`/workspace/lula-auth/ref/desde0-backend/src/auth/
/// mod.rs`).
#[tokio::test]
async fn a_rejected_token_gets_a_coarse_body_not_the_validation_detail() {
    let app = TestApp::with_auth_mode(hs256_config(Some("right-secret"))).await;
    let token = sign_hs256("wrong-secret", &Uuid::new_v4().to_string(), ISS, AUD);
    let (status, body) = app
        .request_with_bearer(&token, Method::GET, "/opportunities", None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"]["message"], "invalid token", "{body}");
}

#[tokio::test]
async fn a_missing_token_gets_its_own_coarse_message() {
    let app = TestApp::with_auth_mode(jwks_config("http://127.0.0.1:1/unused", ISS)).await;
    let (status, body) = app.get("/opportunities").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"]["message"], "missing token", "{body}");
}

// ── aal2 gate (plan B6) ──────────────────────────────────────────

#[tokio::test]
async fn aal1_token_is_refused_mfa_required_on_a_workspace_route() {
    let secret = "s";
    let app = TestApp::with_auth_mode(hs256_config(Some(secret))).await;
    let token = sign_hs256_with_aal(secret, &Uuid::new_v4().to_string(), ISS, AUD, Some("aal1"));
    let (status, body) = app
        .request_with_bearer(&token, Method::GET, "/opportunities", None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "mfa_required", "{body}");
}

#[tokio::test]
async fn a_token_with_no_aal_claim_at_all_is_refused_mfa_required() {
    let secret = "s";
    let app = TestApp::with_auth_mode(hs256_config(Some(secret))).await;
    let token = sign_hs256_with_aal(secret, &Uuid::new_v4().to_string(), ISS, AUD, None);
    let (status, body) = app
        .request_with_bearer(&token, Method::GET, "/opportunities", None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "mfa_required", "{body}");
}

#[tokio::test]
async fn aal2_token_is_accepted_on_a_workspace_route() {
    let secret = "s";
    let app = TestApp::with_auth_mode(hs256_config(Some(secret))).await;
    let token = sign_hs256_with_aal(secret, &Uuid::new_v4().to_string(), ISS, AUD, Some("aal2"));
    let (status, body) = app
        .request_with_bearer(&token, Method::GET, "/opportunities", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// `GET /me` is the one exemption: an `aal1` token (first sign-in, before
/// TOTP enrollment) must still be able to reach it so the frontend can
/// learn it needs to enroll.
#[tokio::test]
async fn me_is_reachable_at_aal1() {
    let secret = "s";
    let app = TestApp::with_auth_mode(hs256_config(Some(secret))).await;
    let token = sign_hs256_with_aal(secret, &Uuid::new_v4().to_string(), ISS, AUD, Some("aal1"));
    let (status, body) = app
        .request_with_bearer(&token, Method::GET, "/me", None)
        .await;
    // No control plane wired into `with_auth_mode`, so this can't reach
    // 200 (it 500s on the missing control pool) -- the point here is only
    // that it is NOT the aal2 gate. `tests/workspaces.rs` covers the full
    // 200 path with a real control plane.
    assert_ne!(status, StatusCode::FORBIDDEN, "{body}");
}

/// Finding 4 (prompt-backend-review.md): the `/me` exemption keys off an
/// exact `parts.uri.path() == "/me"` && `GET` match. Confirm path tricks
/// that might coerce a request onto that exemption -- a trailing slash, a
/// doubled leading slash, percent-encoding the route, or swapping the
/// method -- never let an `aal1` token reach anything beyond what the
/// real `GET /me` already allows at `aal1`: either the request doesn't
/// route at all (404/405), or it does and the `aal2` gate still applies.
/// Built against `app.router` directly (not `TestApp::request*`, which
/// prefixes bare paths with `/w/{workspace_id}`) so the URI on the wire is
/// exactly the string under test.
#[tokio::test]
async fn me_exemption_is_not_fooled_by_path_tricks() {
    let secret = "s";
    let app = TestApp::with_auth_mode(hs256_config(Some(secret))).await;
    let token = sign_hs256_with_aal(secret, &Uuid::new_v4().to_string(), ISS, AUD, Some("aal1"));

    for (method, path) in [
        (Method::GET, "/me/"),
        (Method::GET, "//me"),
        (Method::GET, "/%6De"),
        (Method::HEAD, "/me"),
    ] {
        let request = Request::builder()
            .method(method.clone())
            .uri(path)
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let response = app.router.clone().oneshot(request).await.unwrap();
        assert_ne!(
            response.status(),
            StatusCode::OK,
            "{method} {path} must not bypass the aal2 gate via the /me exemption"
        );
    }
}

/// The router-walk test (plan B6): every route the API registers, except
/// the public ones and `GET /me`, must refuse an `aal1` token with 403
/// `mfa_required` -- enumerated from the live OpenAPI document (the same
/// one `GET /openapi.json` serves) so a new route can't be added here
/// without this test seeing it.
#[tokio::test]
async fn every_non_public_non_me_route_requires_aal2() {
    let secret = "s";
    let app = TestApp::with_auth_mode(hs256_config(Some(secret))).await;
    let token = sign_hs256_with_aal(secret, &Uuid::new_v4().to_string(), ISS, AUD, Some("aal1"));

    let doc = lula_dog_backend::ApiDoc::openapi();
    assert!(
        doc.paths.paths.len() > 20,
        "the gate is not seeing the real route table"
    );

    let dummy = Uuid::new_v4().to_string();
    let mut checked = 0;
    for (path, item) in &doc.paths.paths {
        let concrete = path
            .split('/')
            .map(|seg| {
                if seg.starts_with('{') && seg.ends_with('}') {
                    dummy.as_str()
                } else {
                    seg
                }
            })
            .collect::<Vec<_>>()
            .join("/");

        for (method, present) in [
            (Method::GET, item.get.is_some()),
            (Method::POST, item.post.is_some()),
            (Method::PUT, item.put.is_some()),
            (Method::PATCH, item.patch.is_some()),
            (Method::DELETE, item.delete.is_some()),
        ] {
            if !present {
                continue;
            }
            let (status, body) = app
                .request_with_bearer(&token, method.clone(), &concrete, Some(json!({})))
                .await;

            if lula_dog_backend::auth::is_public(path) {
                assert_ne!(
                    status,
                    StatusCode::FORBIDDEN,
                    "public route {method} {path} must not be aal2-gated: {body}"
                );
                continue;
            }
            if method == Method::GET && path == "/me" {
                assert_ne!(
                    status,
                    StatusCode::FORBIDDEN,
                    "GET /me must be exempt from the aal2 gate: {body}"
                );
                continue;
            }
            checked += 1;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{method} {path} should refuse an aal1 token: {body}"
            );
            assert_eq!(
                body["error"]["code"], "mfa_required",
                "{method} {path} returned 403 for the wrong reason: {body}"
            );
        }
    }
    assert!(
        checked > 20,
        "the gate exercised too few routes ({checked}) to be load-bearing"
    );
}
