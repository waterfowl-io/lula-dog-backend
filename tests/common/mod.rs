//! Shared harness: a fresh SQLite file per test, migrations applied by the
//! same `db::connect` the server uses, and the router driven in-process.

#![allow(dead_code, reason = "each test crate uses a different subset")]
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test harness: a failure aborting the test is the mechanism"
)]

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use lula_dog_backend::extractor::{Extractor, StubExtractor};
use lula_dog_backend::{AppState, db, router};
use serde_json::Value;
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;

pub struct TestApp {
    pub router: Router,
    pub pool: SqlitePool,
    // Held so the database file outlives the test.
    _dir: TempDir,
}

impl TestApp {
    pub async fn new() -> Self {
        Self::with_extractor(Arc::new(StubExtractor)).await
    }

    pub async fn with_extractor(extractor: Arc<dyn Extractor>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("test.db").display());
        let pool = db::connect(&url).await.unwrap();
        let router = router(AppState {
            pool: pool.clone(),
            extractor,
        });
        Self {
            router,
            pool,
            _dir: dir,
        }
    }

    pub async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let builder = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        let body = body.map_or_else(Body::empty, |json| Body::from(json.to_string()));
        let response = self
            .router
            .clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| panic!("non-JSON body: {}", String::from_utf8_lossy(&bytes)))
        };
        (status, json)
    }

    pub async fn get(&self, path: &str) -> (StatusCode, Value) {
        self.request(Method::GET, path, None).await
    }

    pub async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.request(Method::POST, path, Some(body)).await
    }

    pub async fn patch(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.request(Method::PATCH, path, Some(body)).await
    }

    pub async fn delete(&self, path: &str) -> (StatusCode, Value) {
        self.request(Method::DELETE, path, None).await
    }

    /// Create an opportunity and return its id.
    pub async fn create_opportunity(&self, body: Value) -> i64 {
        let (status, json) = self.post("/opportunities", body).await;
        assert_eq!(status, StatusCode::CREATED, "{json}");
        json["id"].as_i64().unwrap()
    }
}

/// `YYYY-MM-DD HH:MM:SS` UTC, `days` before now (negative for the future).
pub fn days_ago(days: i64) -> String {
    lula_dog_backend::time::format(&(chrono::Utc::now() - chrono::Duration::days(days)))
}
