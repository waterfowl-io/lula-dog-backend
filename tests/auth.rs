//! JWT verification (HS256, and ES256/RS256 against a loopback JWKS
//! server) and the `AUTH_MODE=dev` bypass. No real network: the JWKS
//! "server" is a plain `TcpListener` on `127.0.0.1:0` serving a canned
//! response.

#![expect(
    clippy::unwrap_used,
    reason = "a failing assertion aborting the test is the mechanism"
)]

mod common;

use axum::http::{Method, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use common::TestApp;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use lula_dog_backend::config::AuthMode;
use p256::ecdsa::SigningKey;
use p256::elliptic_curve::pkcs8::EncodePrivateKey;
use p256::pkcs8::LineEnding;
use rsa::RsaPrivateKey;
use rsa::traits::PublicKeyParts;
use serde::Serialize;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
}

fn claims(sub: &str, email: &str, iss: &str, aud: &str, exp_in_future: bool) -> String {
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
    })
    .unwrap()
}

fn sign_hs256(secret: &str, sub: &str, iss: &str, aud: &str) -> String {
    let header = Header::new(Algorithm::HS256);
    let payload: serde_json::Value =
        serde_json::from_str(&claims(sub, "u@test.example", iss, aud, true)).unwrap();
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
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let body = body.clone();
            tokio::spawn(async move {
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
    format!("http://{addr}")
}

fn auth_mode_for(supabase_url: &str, jwt_secret: Option<&str>) -> AuthMode {
    AuthMode::Supabase {
        supabase_url: supabase_url.to_owned(),
        aud: AUD.to_owned(),
        jwt_secret: jwt_secret.map(ToOwned::to_owned),
        jwks_ttl_secs: 300,
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

#[tokio::test]
async fn supabase_mode_rejects_missing_authorization_header() {
    let app = TestApp::with_auth_mode(auth_mode_for("http://127.0.0.1:1", None)).await;
    let (status, body) = app.get("/opportunities").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

#[tokio::test]
async fn supabase_mode_rejects_a_non_bearer_header() {
    let app = TestApp::with_auth_mode(auth_mode_for("http://127.0.0.1:1", Some("secret"))).await;
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
    let app = TestApp::with_auth_mode(auth_mode_for(
        ISS.trim_end_matches("/auth/v1"),
        Some(secret),
    ))
    .await;
    let token = sign_hs256(secret, &Uuid::new_v4().to_string(), ISS, AUD);
    let (status, body) = app
        .request_with_bearer(&token, Method::GET, "/opportunities", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn hs256_wrong_secret_is_rejected() {
    let app = TestApp::with_auth_mode(auth_mode_for(
        ISS.trim_end_matches("/auth/v1"),
        Some("right-secret"),
    ))
    .await;
    let token = sign_hs256("wrong-secret", &Uuid::new_v4().to_string(), ISS, AUD);
    let (status, body) = app
        .request_with_bearer(&token, Method::GET, "/opportunities", None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

#[tokio::test]
async fn hs256_wrong_audience_is_rejected() {
    let secret = "s";
    let app = TestApp::with_auth_mode(auth_mode_for(
        ISS.trim_end_matches("/auth/v1"),
        Some(secret),
    ))
    .await;
    let token = sign_hs256(secret, &Uuid::new_v4().to_string(), ISS, "some-other-aud");
    let (status, body) = app
        .request_with_bearer(&token, Method::GET, "/opportunities", None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

#[tokio::test]
async fn hs256_wrong_issuer_is_rejected() {
    let secret = "s";
    let app = TestApp::with_auth_mode(auth_mode_for(
        ISS.trim_end_matches("/auth/v1"),
        Some(secret),
    ))
    .await;
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
    let app = TestApp::with_auth_mode(auth_mode_for(
        ISS.trim_end_matches("/auth/v1"),
        Some(secret),
    ))
    .await;
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
    let app = TestApp::with_auth_mode(auth_mode_for(
        ISS.trim_end_matches("/auth/v1"),
        Some(secret),
    ))
    .await;
    let token = sign_hs256(secret, "not-a-uuid", ISS, AUD);
    let (status, body) = app
        .request_with_bearer(&token, Method::GET, "/opportunities", None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

#[tokio::test]
async fn hs256_without_a_configured_secret_is_rejected() {
    let secret = "s";
    let app = TestApp::with_auth_mode(auth_mode_for(ISS.trim_end_matches("/auth/v1"), None)).await;
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
    let app = TestApp::with_auth_mode(auth_mode_for(&base_url, None)).await;

    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some("test-es256".to_owned());
    let iss = format!("{base_url}/auth/v1");
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
    let app = TestApp::with_auth_mode(auth_mode_for(&base_url, None)).await;

    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("test-rs256".to_owned());
    let iss = format!("{base_url}/auth/v1");
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
    let app = TestApp::with_auth_mode(auth_mode_for(&base_url, None)).await;

    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some("missing".to_owned());
    let signing_key = SigningKey::random(&mut rand::rngs::OsRng);
    let pkcs8 = signing_key.to_pkcs8_pem(LineEnding::LF).unwrap();
    let iss = format!("{base_url}/auth/v1");
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
