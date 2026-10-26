//! `/me`, `/workspaces`, and `/w/{workspace_id}` membership enforcement.
//! All gated on `TEST_CONTROL_DATABASE_URL`: `TestApp::with_control` skips
//! (with a printed note) when it isn't set, so the rest of the suite never
//! needs Postgres. Each test creates its own profile/workspace with random
//! ids, so they're safe to run in parallel against a shared database.

#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "a failing assertion aborting the test is the mechanism"
)]

mod common;

use axum::http::{Method, StatusCode};
use common::TestApp;
use lula_dog_backend::control::profiles;
use lula_dog_backend::control::workspaces::{self, Role};
use serde_json::{Value, json};
use utoipa::OpenApi as _;
use uuid::Uuid;

/// Add `user_id` to `app`'s default workspace with `role`, provisioning
/// their profile first (membership rows reference `profiles`).
async fn add_member(app: &TestApp, user_id: Uuid, role: Role) {
    let control = app.control.as_ref().unwrap();
    profiles::upsert_from_jwt(control, user_id, &format!("{user_id}@test.example"))
        .await
        .unwrap();
    workspaces::set_role(control, app.workspace_id, user_id, role)
        .await
        .unwrap();
}

#[tokio::test]
async fn me_returns_profile_and_memberships() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let (status, body) = app.get("/me").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["profile"]["id"], app.dev_user_id.to_string());
    let workspaces = body["workspaces"].as_array().unwrap();
    assert!(
        workspaces
            .iter()
            .any(|w| w["id"] == app.workspace_id.to_string()),
        "{body}"
    );
}

#[tokio::test]
async fn create_workspace_makes_the_caller_owner_and_a_sqlite_file() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let (status, created) = app
        .post("/workspaces", json!({"name": "A New Workspace"}))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let workspace_id = created["id"].as_str().unwrap();

    let (status, list) = app.get("/workspaces").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let memberships = list.as_array().unwrap();
    let membership = memberships
        .iter()
        .find(|w| w["id"] == workspace_id)
        .unwrap_or_else(|| panic!("new workspace not in {list}"));
    assert_eq!(membership["role"], "owner");
}

#[tokio::test]
async fn create_workspace_rejects_an_empty_name() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let (status, body) = app.post("/workspaces", json!({"name": "   "})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn non_member_is_forbidden() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let outsider = Uuid::new_v4();
    let (status, body) = app
        .request_as(outsider, Method::GET, "/opportunities", None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn unknown_workspace_is_forbidden_not_found() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    // A membership check happens before the workspace itself is looked up,
    // so a workspace id nobody owns reads the same as one that doesn't
    // exist: forbidden, not a 404 that would leak which ids are valid.
    let bogus_workspace = format!("/w/{}/opportunities", Uuid::new_v4());
    let (status, body) = app.get(&bogus_workspace).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn viewer_role_can_read_but_not_write() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let control = app.control.as_ref().unwrap();
    let viewer = Uuid::new_v4();
    lula_dog_backend::control::profiles::upsert_from_jwt(control, viewer, "viewer@test.example")
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, 'viewer')",
    )
    .bind(app.workspace_id)
    .bind(viewer)
    .execute(control)
    .await
    .unwrap();

    let (status, body) = app
        .request_as(viewer, Method::GET, "/opportunities", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = app
        .request_as(
            viewer,
            Method::POST,
            "/opportunities",
            Some(json!({"company": "Viewer Co"})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

/// A valid-enough body for routes `members.rs` parses a required-field
/// body for (`ApiJson` runs as part of the handler's own extraction, after
/// `workspace_db::scope`'s route_layer has already let the request through
/// -- unlike `/w/{workspace_id}` routes, these aren't wrapped by `scope`,
/// so an empty body would 400 on missing fields before `require_admin` is
/// ever reached, masking the 403 this test looks for). Every other route
/// is happy with `{}`, since its own role gate runs before any body
/// parsing.
fn body_for(path: &str, dummy: &str) -> Value {
    match path {
        "/workspaces/{workspace_id}/invites" => {
            json!({"email": "x@test.example", "role": "viewer"})
        }
        "/workspaces/{workspace_id}/members/{user_id}" => json!({"role": "viewer"}),
        "/workspaces/{workspace_id}/transfer-ownership" => json!({"to_user_id": dummy}),
        _ => json!({}),
    }
}

/// Router-walk style test (the matrix update from Nolan, 2026-10-25):
/// every mutating route (any method but GET/HEAD/OPTIONS), enumerated from
/// the live OpenAPI document so a new route can't be added without this
/// test seeing it, must refuse a `viewer` with 403 -- except
/// `POST /workspaces`, which isn't scoped to an existing workspace at all
/// (the caller becomes the new workspace's `owner`, regardless of their
/// role anywhere else).
#[tokio::test]
async fn viewer_gets_403_on_every_mutating_route() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let control = app.control.as_ref().unwrap();
    let viewer = Uuid::new_v4();
    lula_dog_backend::control::profiles::upsert_from_jwt(
        control,
        viewer,
        "walk-viewer@test.example",
    )
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, 'viewer')",
    )
    .bind(app.workspace_id)
    .bind(viewer)
    .execute(control)
    .await
    .unwrap();

    let doc = lula_dog_backend::ApiDoc::openapi();
    assert!(
        doc.paths.paths.len() > 20,
        "the walk is not seeing the real route table"
    );

    let dummy = Uuid::new_v4().to_string();
    let mut checked = 0;
    for (path, item) in &doc.paths.paths {
        if path == "/workspaces" {
            continue;
        }
        let concrete = path
            .replace("{workspace_id}", &app.workspace_id.to_string())
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
            (Method::POST, item.post.is_some()),
            (Method::PUT, item.put.is_some()),
            (Method::PATCH, item.patch.is_some()),
            (Method::DELETE, item.delete.is_some()),
        ] {
            if !present {
                continue;
            }
            checked += 1;
            let (status, body) = app
                .request_as(
                    viewer,
                    method.clone(),
                    &concrete,
                    Some(body_for(path, &dummy)),
                )
                .await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{method} {path} should refuse a viewer: {body}"
            );
        }
    }
    assert!(
        checked > 10,
        "the walk exercised too few routes ({checked}) to be load-bearing"
    );
}

/// Finding 6 (prompt-backend-delete.md): Nolan's matrix makes EVERY delete
/// `admin`+, with no carve-out for ordinary workspace data -- the previous
/// run left `DELETE /w/{ws}/tasks/{id}` and `DELETE
/// /w/{ws}/target-profiles/{id}` at `member`+. Router-walk style, like
/// `viewer_gets_403_on_every_mutating_route` above: every DELETE route
/// under `/w/{workspace_id}`, enumerated from the live OpenAPI document,
/// must refuse a `member` with 403.
#[tokio::test]
async fn member_gets_403_on_every_data_plane_delete_route() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let member = Uuid::new_v4();
    add_member(&app, member, Role::Member).await;

    let doc = lula_dog_backend::ApiDoc::openapi();
    let dummy = Uuid::new_v4().to_string();
    let mut checked = 0;
    for (path, item) in &doc.paths.paths {
        if item.delete.is_none() || !path.starts_with("/w/{workspace_id}") {
            continue;
        }
        let concrete = path
            .replace("{workspace_id}", &app.workspace_id.to_string())
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
        checked += 1;
        let (status, body) = app
            .request_as(member, Method::DELETE, &concrete, None)
            .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "DELETE {path} should refuse a member: {body}"
        );
    }
    assert!(
        checked >= 2,
        "the walk exercised too few DELETE routes ({checked}) to be load-bearing"
    );
}

/// Finding 6, the other half: an `admin` actually can delete ordinary
/// workspace data, not just invites/members.
#[tokio::test]
async fn admin_can_delete_a_task_and_a_target_profile_a_member_cannot() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let member = Uuid::new_v4();
    let admin = Uuid::new_v4();
    add_member(&app, member, Role::Member).await;
    add_member(&app, admin, Role::Admin).await;

    let opportunity_id = app
        .create_opportunity(json!({"company": "Delete Co"}))
        .await;
    let (status, task) = app
        .post(
            &format!("/opportunities/{opportunity_id}/tasks"),
            json!({"title": "follow up"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{task}");
    let task_id = task["id"].as_i64().unwrap();

    let (status, profile) = app
        .post(
            "/target-profiles",
            json!({"name": "Delete-able profile", "min_fit_score": 0}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{profile}");
    let profile_id = profile["id"].as_i64().unwrap();

    let (status, body) = app
        .request_as(member, Method::DELETE, &format!("/tasks/{task_id}"), None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = app
        .request_as(
            member,
            Method::DELETE,
            &format!("/target-profiles/{profile_id}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, body) = app
        .request_as(admin, Method::DELETE, &format!("/tasks/{task_id}"), None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (status, body) = app
        .request_as(
            admin,
            Method::DELETE,
            &format!("/target-profiles/{profile_id}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
}

#[tokio::test]
async fn two_workspaces_have_independent_sqlite_data() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    app.create_opportunity(json!({"company": "Workspace One Co"}))
        .await;

    let (status, created) = app
        .post("/workspaces", json!({"name": "Second Workspace"}))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let second_id = created["id"].as_str().unwrap();

    let (status, list) = app.get(&format!("/w/{second_id}/opportunities")).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list.as_array().unwrap().len(), 0, "{list}");
}
