//! Shared harness: a fresh workspace (its own SQLite file under a temp
//! dir) per test, migrations applied by the same `db::connect` the server
//! uses, and the router driven in-process under `AUTH_MODE=dev` with no
//! control plane configured -- so every workspace id is accessible as
//! `owner` with no Postgres needed (see `workspace_db::scope`). Tests that
//! specifically exercise the control plane (membership 403s, `/me`,
//! `/workspaces`, apps, saved queries, the query runner) use
//! `TestApp::with_control`, gated on `TEST_CONTROL_DATABASE_URL`: it skips
//! (printing a note) when that isn't set, so Postgres is never required to
//! run the rest of the suite.

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
use lula_dog_backend::auth::Auth;
use lula_dog_backend::config::AuthMode;
use lula_dog_backend::control::{self, ControlPool};
use lula_dog_backend::extractor::{Extractor, StubExtractor};
use lula_dog_backend::workspace_db::WorkspaceRegistry;
use lula_dog_backend::{AppState, router};
use serde_json::Value;
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

/// Routes with no `/w/{workspace_id}` prefix.
const PUBLIC_PATHS: &[&str] = &["/health", "/openapi.json", "/app-definition.schema.json"];

pub struct TestApp {
    pub router: Router,
    /// The default test workspace's SQLite pool.
    pub pool: SqlitePool,
    pub workspace_id: Uuid,
    pub dev_user_id: Uuid,
    /// `Some` only for `TestApp::with_control`.
    pub control: Option<ControlPool>,
    workspaces: WorkspaceRegistry,
    // Held so the database files outlive the test.
    _dir: TempDir,
}

impl TestApp {
    pub async fn new() -> Self {
        Self::with_extractor(Arc::new(StubExtractor)).await
    }

    pub async fn with_extractor(extractor: Arc<dyn Extractor>) -> Self {
        let dev_user_id = Uuid::new_v4();
        Self::build(
            extractor,
            None,
            AuthMode::Dev {
                default_user_id: dev_user_id,
            },
        )
        .await
    }

    /// With a given `AuthMode` (e.g. `Supabase` pointed at a loopback JWKS
    /// server) and no control plane, so only the auth layer is exercised:
    /// every workspace id is accessible as `owner` regardless of who the
    /// token says the caller is.
    pub async fn with_auth_mode(auth_mode: AuthMode) -> Self {
        Self::build(Arc::new(StubExtractor), None, auth_mode).await
    }

    /// Against a real Postgres control plane (`TEST_CONTROL_DATABASE_URL`),
    /// with a dedicated profile + workspace (random ids, so concurrent
    /// tests never collide) the dev user owns. `None`, with a printed
    /// note, when that env var isn't set.
    pub async fn with_control() -> Option<Self> {
        let Ok(url) = std::env::var("TEST_CONTROL_DATABASE_URL") else {
            println!("TEST_CONTROL_DATABASE_URL not set; skipping a control-plane-backed test");
            return None;
        };
        let control = control::connect(&url).await.unwrap();
        let dev_user_id = Uuid::new_v4();
        let email = format!("{dev_user_id}@test.example");
        let profile = control::profiles::upsert_from_jwt(&control, dev_user_id, &email)
            .await
            .unwrap();
        let workspace_name = format!("test workspace {dev_user_id}");
        let workspace = control::workspaces::create(&control, &workspace_name, profile.id)
            .await
            .unwrap();

        let mut app = Self::build(
            Arc::new(StubExtractor),
            Some(control),
            AuthMode::Dev {
                default_user_id: dev_user_id,
            },
        )
        .await;
        app.workspace_id = workspace.id;
        app.pool = app.workspaces.get_or_create(workspace.id).await.unwrap();
        Some(app)
    }

    async fn build(
        extractor: Arc<dyn Extractor>,
        control: Option<ControlPool>,
        auth_mode: AuthMode,
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let legacy_url = format!("sqlite://{}", dir.path().join("legacy.db").display());
        let workspaces = WorkspaceRegistry::new(dir.path().to_path_buf(), legacy_url);
        let workspace_id = Uuid::new_v4();
        let pool = workspaces.get_or_create(workspace_id).await.unwrap();
        let dev_user_id = match &auth_mode {
            AuthMode::Dev { default_user_id } => *default_user_id,
            AuthMode::Supabase { .. } => Uuid::nil(),
        };
        let auth = Arc::new(Auth::from_config(&auth_mode).unwrap());
        let state = AppState {
            extractor,
            control: control.clone(),
            workspaces: workspaces.clone(),
            auth,
            query_max_rows: 1000,
            query_timeout_ms: 5000,
        };
        let router = router(state);
        Self {
            router,
            pool,
            workspace_id,
            dev_user_id,
            control,
            workspaces,
            _dir: dir,
        }
    }

    /// Prefixes data-plane paths with `/w/{workspace_id}`; public,
    /// control-plane (`/me`, `/workspaces`) and already-prefixed (`/w/...`,
    /// for cross-workspace tests) paths pass through unchanged.
    fn full_path(&self, path: &str) -> String {
        if PUBLIC_PATHS.contains(&path)
            || path.starts_with("/me")
            || path.starts_with("/workspaces")
            || path.starts_with("/w/")
        {
            path.to_owned()
        } else {
            format!("/w/{}{path}", self.workspace_id)
        }
    }

    pub async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        self.request_with_headers(method, path, body, &[]).await
    }

    /// Like `request`, but as a specific dev user (`X-Dev-User`) instead of
    /// this app's default.
    pub async fn request_as(
        &self,
        user_id: Uuid,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        self.request_with_headers(method, path, body, &[("x-dev-user", &user_id.to_string())])
            .await
    }

    /// Like `request`, with a bearer token (for `AuthMode::Supabase` tests).
    pub async fn request_with_bearer(
        &self,
        token: &str,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        self.request_with_headers(
            method,
            path,
            body,
            &[("authorization", &format!("Bearer {token}"))],
        )
        .await
    }

    pub async fn request_with_headers(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(self.full_path(path))
            .header("content-type", "application/json");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
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
