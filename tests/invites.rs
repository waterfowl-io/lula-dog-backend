//! `/workspaces/{id}/invites`, `/workspaces/{id}/members` and
//! `/workspaces/{id}/transfer-ownership` (plan B7), plus invite
//! acceptance on provisioning (plan B4). Gated on `TEST_CONTROL_DATABASE_URL`,
//! like `tests/workspaces.rs`.

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
use serde_json::json;
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
async fn owner_can_invite_viewer_member_or_admin() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    for role in ["viewer", "member", "admin"] {
        let (status, body) = app
            .post(
                &format!("/workspaces/{}/invites", app.workspace_id),
                json!({"email": format!("{role}@test.example"), "role": role}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["role"], role, "{body}");
    }
}

#[tokio::test]
async fn admin_can_invite_but_not_grant_owner() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let admin = Uuid::new_v4();
    add_member(&app, admin, Role::Admin).await;

    let (status, body) = app
        .request_as(
            admin,
            Method::POST,
            &format!("/workspaces/{}/invites", app.workspace_id),
            Some(json!({"email": "new-member@test.example", "role": "member"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, body) = app
        .request_as(
            admin,
            Method::POST,
            &format!("/workspaces/{}/invites", app.workspace_id),
            Some(json!({"email": "wannabe-owner@test.example", "role": "owner"})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn member_and_viewer_cannot_invite_at_all() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let member = Uuid::new_v4();
    let viewer = Uuid::new_v4();
    add_member(&app, member, Role::Member).await;
    add_member(&app, viewer, Role::Viewer).await;

    for actor in [member, viewer] {
        let (status, body) = app
            .request_as(
                actor,
                Method::POST,
                &format!("/workspaces/{}/invites", app.workspace_id),
                Some(json!({"email": "someone@test.example", "role": "viewer"})),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }
}

#[tokio::test]
async fn a_duplicate_pending_invite_is_a_conflict() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let body = json!({"email": "dup@test.example", "role": "member"});
    let (status, resp) = app
        .post(
            &format!("/workspaces/{}/invites", app.workspace_id),
            body.clone(),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{resp}");
    let (status, resp) = app
        .post(&format!("/workspaces/{}/invites", app.workspace_id), body)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{resp}");
}

/// Plan B4: provisioning auto-accepts a pending invite for the VERIFIED
/// email claim. `auth.mode = "dev"` synthesizes `dev+{id}@local.test`, so
/// inviting that exact address for a not-yet-seen id and then making any
/// authenticated request as that id is what "signing in" looks like here.
#[tokio::test]
async fn invite_is_accepted_automatically_on_the_invitees_first_request() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let new_user = Uuid::new_v4();
    let email = format!("dev+{new_user}@local.test");

    let (status, body) = app
        .post(
            &format!("/workspaces/{}/invites", app.workspace_id),
            json!({"email": email, "role": "member"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // Any authenticated request provisions; `/me` is simplest.
    let (status, body) = app.request_as(new_user, Method::GET, "/me", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, members) = app
        .get(&format!("/workspaces/{}/members", app.workspace_id))
        .await;
    assert_eq!(status, StatusCode::OK, "{members}");
    let member = members
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["user_id"] == new_user.to_string())
        .unwrap_or_else(|| panic!("invited user not a member: {members}"));
    assert_eq!(member["role"], "member");

    // Idempotent: a second request must not fail or re-claim anything odd.
    let (status, _) = app.request_as(new_user, Method::GET, "/me", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn an_expired_invite_is_never_accepted() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let control = app.control.as_ref().unwrap();
    let new_user = Uuid::new_v4();
    let email = format!("dev+{new_user}@local.test");
    sqlx::query(
        "INSERT INTO workspace_invites (workspace_id, email, role, expires_at)
         VALUES ($1, $2, 'member', now() - interval '1 hour')",
    )
    .bind(app.workspace_id)
    .bind(&email)
    .execute(control)
    .await
    .unwrap();

    let (status, _) = app.request_as(new_user, Method::GET, "/me", None).await;
    assert_eq!(status, StatusCode::OK);

    let (_, members) = app
        .get(&format!("/workspaces/{}/members", app.workspace_id))
        .await;
    assert!(
        !members
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["user_id"] == new_user.to_string()),
        "{members}"
    );
}

#[tokio::test]
async fn deleting_an_invite_is_workspace_scoped_foreign_ids_404() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let (status, invite) = app
        .post(
            &format!("/workspaces/{}/invites", app.workspace_id),
            json!({"email": "scoped@test.example", "role": "member"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{invite}");
    let invite_id = invite["id"].as_str().unwrap();

    let other_workspace = Uuid::new_v4();
    let (status, body) = app
        .delete(&format!(
            "/workspaces/{other_workspace}/invites/{invite_id}"
        ))
        .await;
    // Not a member of `other_workspace` at all, so the membership gate
    // fires first -- forbidden, not a 404 that would confirm the invite's
    // real workspace.
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn patching_a_member_to_owner_requires_an_owner() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let admin = Uuid::new_v4();
    let member = Uuid::new_v4();
    add_member(&app, admin, Role::Admin).await;
    add_member(&app, member, Role::Member).await;

    let (status, body) = app
        .request_as(
            admin,
            Method::PATCH,
            &format!("/workspaces/{}/members/{member}", app.workspace_id),
            Some(json!({"role": "owner"})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, body) = app
        .patch(
            &format!("/workspaces/{}/members/{member}", app.workspace_id),
            json!({"role": "owner"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["role"], "owner");
}

#[tokio::test]
async fn the_last_owner_cannot_be_demoted_or_removed() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    // `app.dev_user_id` is the sole owner (TestApp::with_control makes the
    // creator the owner of a fresh workspace).
    let (status, body) = app
        .patch(
            &format!(
                "/workspaces/{}/members/{}",
                app.workspace_id, app.dev_user_id
            ),
            json!({"role": "admin"}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    let (status, body) = app
        .delete(&format!(
            "/workspaces/{}/members/{}",
            app.workspace_id, app.dev_user_id
        ))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[tokio::test]
async fn demoting_an_owner_is_fine_when_another_owner_remains() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let second_owner = Uuid::new_v4();
    add_member(&app, second_owner, Role::Owner).await;

    let (status, body) = app
        .patch(
            &format!("/workspaces/{}/members/{second_owner}", app.workspace_id),
            json!({"role": "admin"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["role"], "admin");
}

#[tokio::test]
async fn transfer_ownership_moves_owner_and_demotes_the_caller() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let new_owner = Uuid::new_v4();
    add_member(&app, new_owner, Role::Admin).await;

    let (status, body) = app
        .post(
            &format!("/workspaces/{}/transfer-ownership", app.workspace_id),
            json!({"to_user_id": new_owner}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["role"], "owner");

    let (_, members) = app
        .get(&format!("/workspaces/{}/members", app.workspace_id))
        .await;
    let members = members.as_array().unwrap();
    let old_owner = members
        .iter()
        .find(|m| m["user_id"] == app.dev_user_id.to_string())
        .unwrap();
    assert_eq!(old_owner["role"], "admin");
}

#[tokio::test]
async fn transfer_ownership_requires_the_caller_to_already_be_an_owner() {
    let Some(app) = TestApp::with_control().await else {
        return;
    };
    let admin = Uuid::new_v4();
    let other = Uuid::new_v4();
    add_member(&app, admin, Role::Admin).await;
    add_member(&app, other, Role::Member).await;

    let (status, body) = app
        .request_as(
            admin,
            Method::POST,
            &format!("/workspaces/{}/transfer-ownership", app.workspace_id),
            Some(json!({"to_user_id": other})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}
