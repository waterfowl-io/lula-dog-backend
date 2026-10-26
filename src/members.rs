//! `/workspaces/{id}/invites`, `/workspaces/{id}/members` and
//! `/workspaces/{id}/transfer-ownership` (plan B7). Sibling to
//! `/workspaces` (`crate::me`), not nested under `/w/{workspace_id}`: these
//! manage the workspace itself, not its data, so they are gated on
//! membership here rather than by `workspace_db::scope`.
//!
//! Every route here is admin-or-above; owner-touching changes (granting or
//! removing `owner`, and the transfer itself) additionally require the
//! caller to already be an owner -- see `Role::can_assign`. A change that
//! would leave the workspace with no owner at all is refused with 409
//! regardless of who asks.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::AppState;
use crate::auth::AuthUser;
use crate::control::ControlPool;
use crate::control::invites::{self, Invite, NewInvite};
use crate::control::workspaces::{self, Member, Role};
use crate::error::{AppError, AppResult};
use crate::extract::ApiJson;
use crate::workspace_db;

fn control(state: &AppState) -> AppResult<&ControlPool> {
    state
        .control
        .as_ref()
        .ok_or_else(|| AppError::Internal("CONTROL_DATABASE_URL is not configured".to_owned()))
}

/// Resolve the caller's role and require at least `admin` -- the baseline
/// gate every route in this module shares (share/unshare and delete; see
/// `Role`'s doc comment for the full matrix), via the same `Role::require`
/// every other gate in the codebase uses. A non-member gets the same 403
/// `workspace_db::scope` gives a non-member of a data-plane route (not a
/// 404 that would confirm the workspace exists); a member below `admin`
/// gets the same `Role::require` rejection naming the actual reason.
async fn require_admin(state: &AppState, workspace_id: Uuid, user_id: Uuid) -> AppResult<Role> {
    let role = workspace_db::resolve_role(state, workspace_id, user_id).await?;
    role.require(Role::Admin)?;
    Ok(role)
}

#[derive(Deserialize)]
pub struct WorkspaceIdParam {
    workspace_id: Uuid,
}

#[derive(Deserialize)]
pub struct InviteIdParam {
    workspace_id: Uuid,
    invite_id: Uuid,
}

#[derive(Deserialize)]
pub struct MemberIdParam {
    workspace_id: Uuid,
    user_id: Uuid,
}

/// List the workspace's still-pending invites.
#[utoipa::path(
    get,
    path = "/workspaces/{workspace_id}/invites",
    params(("workspace_id" = Uuid, Path)),
    responses(
        (status = 200, body = [Invite]),
        (status = 403, body = crate::error::ErrorBody),
    ),
    tag = "members"
)]
pub async fn list_invites(
    State(state): State<AppState>,
    user: AuthUser,
    Path(WorkspaceIdParam { workspace_id }): Path<WorkspaceIdParam>,
) -> AppResult<Json<Vec<Invite>>> {
    require_admin(&state, workspace_id, user.id).await?;
    Ok(Json(
        invites::list_pending(control(&state)?, workspace_id).await?,
    ))
}

/// Invite someone by email. An admin may invite `viewer`/`member`/`admin`;
/// only an owner may invite another `owner`. 409s if this email already
/// belongs to a member of the workspace -- an invite is for someone who
/// isn't here yet; changing an existing member's role goes through
/// `PATCH /workspaces/{id}/members/{user_id}` instead, which (unlike
/// invite acceptance) runs `Role::can_assign` and the last-owner guard.
#[utoipa::path(
    post,
    path = "/workspaces/{workspace_id}/invites",
    params(("workspace_id" = Uuid, Path)),
    request_body = NewInvite,
    responses(
        (status = 201, body = Invite),
        (status = 400, body = crate::error::ErrorBody),
        (status = 403, body = crate::error::ErrorBody),
        (status = 409, body = crate::error::ErrorBody),
    ),
    tag = "members"
)]
pub async fn create_invite(
    State(state): State<AppState>,
    user: AuthUser,
    Path(WorkspaceIdParam { workspace_id }): Path<WorkspaceIdParam>,
    ApiJson(new): ApiJson<NewInvite>,
) -> AppResult<(StatusCode, Json<Invite>)> {
    let actor_role = require_admin(&state, workspace_id, user.id).await?;
    let email = new.email.trim();
    if email.is_empty() {
        return Err(AppError::BadRequest("email cannot be empty".to_owned()));
    }
    if !Role::can_assign(actor_role, None, Some(new.role)) {
        return Err(AppError::Forbidden(format!(
            "the {} role cannot invite a {}",
            actor_role.as_str(),
            new.role.as_str()
        )));
    }
    let control = control(&state)?;
    if workspaces::member_with_email_exists(control, workspace_id, email).await? {
        return Err(AppError::Conflict(
            "this email already belongs to a member of this workspace".to_owned(),
        ));
    }
    let invite = invites::create(control, workspace_id, user.id, email, new.role)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.is_unique_violation() => AppError::Conflict(
                "a pending invite already exists for this email in this workspace".to_owned(),
            ),
            _ => AppError::Database(e),
        })?;
    Ok((StatusCode::CREATED, Json(invite)))
}

/// Revoke a pending invite. Removing an `owner`-role invite requires an
/// owner, the same as removing an `owner` member does.
#[utoipa::path(
    delete,
    path = "/workspaces/{workspace_id}/invites/{invite_id}",
    params(("workspace_id" = Uuid, Path), ("invite_id" = Uuid, Path)),
    responses(
        (status = 204, description = "Revoked"),
        (status = 403, body = crate::error::ErrorBody),
        (status = 404, body = crate::error::ErrorBody),
    ),
    tag = "members"
)]
pub async fn delete_invite(
    State(state): State<AppState>,
    user: AuthUser,
    Path(InviteIdParam {
        workspace_id,
        invite_id,
    }): Path<InviteIdParam>,
) -> AppResult<StatusCode> {
    let actor_role = require_admin(&state, workspace_id, user.id).await?;
    let control = control(&state)?;
    let invite = invites::fetch(control, workspace_id, invite_id)
        .await?
        .ok_or(AppError::NotFound("invite"))?;
    if !Role::can_assign(actor_role, Some(invite.role), None) {
        return Err(AppError::Forbidden(format!(
            "the {} role cannot remove a pending {} invite",
            actor_role.as_str(),
            invite.role.as_str()
        )));
    }
    invites::delete(control, workspace_id, invite_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// List the workspace's members.
#[utoipa::path(
    get,
    path = "/workspaces/{workspace_id}/members",
    params(("workspace_id" = Uuid, Path)),
    responses(
        (status = 200, body = [Member]),
        (status = 403, body = crate::error::ErrorBody),
    ),
    tag = "members"
)]
pub async fn list_members(
    State(state): State<AppState>,
    user: AuthUser,
    Path(WorkspaceIdParam { workspace_id }): Path<WorkspaceIdParam>,
) -> AppResult<Json<Vec<Member>>> {
    require_admin(&state, workspace_id, user.id).await?;
    Ok(Json(
        workspaces::list_members(control(&state)?, workspace_id).await?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RoleChange {
    pub role: Role,
}

/// Change a member's role. Refused with 409 if it would leave the
/// workspace with no owner at all.
#[utoipa::path(
    patch,
    path = "/workspaces/{workspace_id}/members/{user_id}",
    params(("workspace_id" = Uuid, Path), ("user_id" = Uuid, Path)),
    request_body = RoleChange,
    responses(
        (status = 200, body = Member),
        (status = 403, body = crate::error::ErrorBody),
        (status = 404, body = crate::error::ErrorBody),
        (status = 409, body = crate::error::ErrorBody),
    ),
    tag = "members"
)]
pub async fn update_member_role(
    State(state): State<AppState>,
    user: AuthUser,
    Path(MemberIdParam {
        workspace_id,
        user_id,
    }): Path<MemberIdParam>,
    ApiJson(req): ApiJson<RoleChange>,
) -> AppResult<Json<Member>> {
    let actor_role = require_admin(&state, workspace_id, user.id).await?;
    let control = control(&state)?;
    let current = workspaces::role_for(control, workspace_id, user_id)
        .await?
        .ok_or(AppError::NotFound("member"))?;
    if !Role::can_assign(actor_role, Some(current), Some(req.role)) {
        return Err(AppError::Forbidden(format!(
            "the {} role cannot change a {} member to {}",
            actor_role.as_str(),
            current.as_str(),
            req.role.as_str()
        )));
    }
    if current == Role::Owner
        && req.role != Role::Owner
        && workspaces::owner_count_excluding(control, workspace_id, user_id).await? == 0
    {
        return Err(AppError::Conflict(
            "the workspace must keep at least one owner".to_owned(),
        ));
    }
    workspaces::set_role(control, workspace_id, user_id, req.role).await?;
    Ok(Json(
        workspaces::list_members(control, workspace_id)
            .await?
            .into_iter()
            .find(|m| m.user_id == user_id)
            .ok_or(AppError::NotFound("member"))?,
    ))
}

/// Remove a member. Refused with 409 if it would leave the workspace with
/// no owner at all.
#[utoipa::path(
    delete,
    path = "/workspaces/{workspace_id}/members/{user_id}",
    params(("workspace_id" = Uuid, Path), ("user_id" = Uuid, Path)),
    responses(
        (status = 204, description = "Removed"),
        (status = 403, body = crate::error::ErrorBody),
        (status = 404, body = crate::error::ErrorBody),
        (status = 409, body = crate::error::ErrorBody),
    ),
    tag = "members"
)]
pub async fn remove_member(
    State(state): State<AppState>,
    user: AuthUser,
    Path(MemberIdParam {
        workspace_id,
        user_id,
    }): Path<MemberIdParam>,
) -> AppResult<StatusCode> {
    let actor_role = require_admin(&state, workspace_id, user.id).await?;
    let control = control(&state)?;
    let current = workspaces::role_for(control, workspace_id, user_id)
        .await?
        .ok_or(AppError::NotFound("member"))?;
    if !Role::can_assign(actor_role, Some(current), None) {
        return Err(AppError::Forbidden(format!(
            "the {} role cannot remove a {} member",
            actor_role.as_str(),
            current.as_str()
        )));
    }
    if current == Role::Owner
        && workspaces::owner_count_excluding(control, workspace_id, user_id).await? == 0
    {
        return Err(AppError::Conflict(
            "the workspace must keep at least one owner".to_owned(),
        ));
    }
    workspaces::remove_member(control, workspace_id, user_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TransferOwnership {
    pub to_user_id: Uuid,
}

/// Transfer ownership to another member: the caller (an owner) becomes
/// `admin`, `to_user_id` becomes `owner`. Only an owner may call this --
/// transferring is granting `owner`, the same rule `Role::can_assign`
/// applies everywhere else.
#[utoipa::path(
    post,
    path = "/workspaces/{workspace_id}/transfer-ownership",
    params(("workspace_id" = Uuid, Path)),
    request_body = TransferOwnership,
    responses(
        (status = 200, body = Member),
        (status = 400, body = crate::error::ErrorBody),
        (status = 403, body = crate::error::ErrorBody),
        (status = 404, body = crate::error::ErrorBody),
    ),
    tag = "members"
)]
pub async fn transfer_ownership(
    State(state): State<AppState>,
    user: AuthUser,
    Path(WorkspaceIdParam { workspace_id }): Path<WorkspaceIdParam>,
    ApiJson(req): ApiJson<TransferOwnership>,
) -> AppResult<Json<Member>> {
    let actor_role = require_admin(&state, workspace_id, user.id).await?;
    if !Role::can_assign(actor_role, Some(Role::Owner), Some(Role::Owner)) {
        return Err(AppError::Forbidden(
            "transferring ownership requires the owner role".to_owned(),
        ));
    }
    if req.to_user_id == user.id {
        return Err(AppError::BadRequest(
            "cannot transfer ownership to yourself".to_owned(),
        ));
    }
    let control = control(&state)?;
    workspaces::role_for(control, workspace_id, req.to_user_id)
        .await?
        .ok_or(AppError::NotFound("member"))?;
    workspaces::transfer_ownership(control, workspace_id, user.id, req.to_user_id).await?;
    Ok(Json(
        workspaces::list_members(control, workspace_id)
            .await?
            .into_iter()
            .find(|m| m.user_id == req.to_user_id)
            .ok_or(AppError::NotFound("member"))?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table B7 asks for: who may make which role change. `None` is
    /// "no membership yet" (invite) or "membership removed"; the pair
    /// `(from, to)` covers invites (`from = None`), role changes (both
    /// `Some`), removal (`to = None`) and transfer-ownership
    /// (`from = to = Some(Owner)`) alike.
    #[test]
    fn can_assign_table() {
        let cases: &[(Role, Option<Role>, Option<Role>, bool)] = &[
            // Viewers and members may not change anyone's role at all.
            (Role::Viewer, None, Some(Role::Member), false),
            (Role::Member, None, Some(Role::Member), false),
            // An admin may invite/set/remove viewer, member or admin.
            (Role::Admin, None, Some(Role::Viewer), true),
            (Role::Admin, None, Some(Role::Member), true),
            (Role::Admin, None, Some(Role::Admin), true),
            (Role::Admin, Some(Role::Viewer), Some(Role::Admin), true),
            (Role::Admin, Some(Role::Admin), None, true),
            // An admin may not touch owner in either direction.
            (Role::Admin, None, Some(Role::Owner), false),
            (Role::Admin, Some(Role::Owner), Some(Role::Admin), false),
            (Role::Admin, Some(Role::Owner), None, false),
            // Only an owner may grant or remove owner -- including
            // transfer-ownership, modeled as owner -> owner.
            (Role::Owner, None, Some(Role::Owner), true),
            (Role::Owner, Some(Role::Owner), Some(Role::Admin), true),
            (Role::Owner, Some(Role::Owner), None, true),
            (Role::Owner, Some(Role::Owner), Some(Role::Owner), true),
            // An owner may also do everything an admin can.
            (Role::Owner, None, Some(Role::Member), true),
            (Role::Owner, Some(Role::Member), None, true),
        ];
        for (actor, from, to, expected) in cases.iter().copied() {
            assert_eq!(
                Role::can_assign(actor, from, to),
                expected,
                "can_assign({actor:?}, {from:?}, {to:?}) should be {expected}"
            );
        }
    }
}
