//! Workspaces and membership. A workspace's editable data lives in its own
//! SQLite file (`crate::workspace_db`), not here.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use super::ControlPool;
use crate::error::{AppError, AppResult};

/// Ordered least to most privileged. The matrix (Nolan, 2026-10-25,
/// superseding the plan's original one):
///
/// - `viewer`: read-only. No data writes, no chat (data or build mode), no
///   publish/discard/rollback.
/// - `member`: `viewer` + write workspace data, build-mode chat, publish,
///   discard and roll back app versions.
/// - `admin`: `member` + share/unshare (invite, remove and change the role
///   of `viewer`/`member`/`admin` members -- see `can_assign`) + EVERY
///   delete, full stop: invites, members, and ordinary workspace data
///   (tasks, target profiles, ...) alike -- see `crate::members` and
///   `docs/architecture.md`'s DELETE-route table.
/// - `owner`: `admin` + the only role that may grant or remove `owner` or
///   call `transfer-ownership` (`can_assign`). A workspace always keeps at
///   least one owner (`owner_count_excluding`). Deleting the whole
///   workspace, if that capability existed, would stay owner-only; no such
///   endpoint exists yet.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, sqlx::Type, ToSchema,
)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum Role {
    Viewer,
    Member,
    Admin,
    Owner,
}

impl Role {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Member => "member",
            Self::Viewer => "viewer",
        }
    }

    /// The one minimum-role check every gate in the codebase goes through --
    /// `workspace_db::scope` (workspace data writes, build-mode chat and
    /// publish/discard/rollback need `member`+; a DELETE needs `admin`+),
    /// `members::require_admin` (`admin`+) and the chat build-mode gate
    /// alike -- so none of them can quietly compare roles differently from
    /// the others.
    pub fn require(self, min: Self) -> AppResult<()> {
        if self < min {
            return Err(AppError::Forbidden(format!(
                "the {} role or above is required",
                min.as_str()
            )));
        }
        Ok(())
    }

    /// Can `actor` make this role change? `from`/`to` are `None` for "no
    /// membership yet" (an invite) and "membership removed" respectively,
    /// so one function covers inviting, role changes and removal alike.
    ///
    /// `admin` may invite, promote, demote or remove `viewer`/`member`/
    /// `admin` -- the "share/unshare" half of the matrix above. Only
    /// `owner` may grant or remove `owner`, which also covers
    /// `transfer-ownership`, since transferring is granting `owner` to
    /// someone else. Below `admin`, nothing is assignable. This says
    /// nothing about whether the change would leave the workspace with no
    /// owner at all; that needs a membership count the caller has, not a
    /// pair of roles -- see `owner_count_excluding`.
    #[must_use]
    pub fn can_assign(actor: Self, from: Option<Self>, to: Option<Self>) -> bool {
        if actor < Self::Admin {
            return false;
        }
        let touches_owner = from == Some(Self::Owner) || to == Some(Self::Owner);
        !touches_owner || actor == Self::Owner
    }
}

#[derive(Debug, Clone, Serialize, FromRow, ToSchema)]
pub struct Workspace {
    pub id: Uuid,
    pub name: String,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A workspace plus the caller's role in it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WorkspaceMembership {
    #[serde(flatten)]
    pub workspace: Workspace,
    pub role: Role,
}

/// The caller's role in a workspace, or `None` if they are not a member.
pub async fn role_for(
    pool: &ControlPool,
    workspace_id: Uuid,
    user_id: Uuid,
) -> AppResult<Option<Role>> {
    Ok(sqlx::query_scalar::<_, Role>(
        "SELECT role FROM workspace_members WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(workspace_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?)
}

#[derive(FromRow)]
struct MembershipRow {
    #[sqlx(flatten)]
    workspace: Workspace,
    role: Role,
}

/// Every workspace `user_id` belongs to, oldest first.
pub async fn list_for_user(
    pool: &ControlPool,
    user_id: Uuid,
) -> AppResult<Vec<WorkspaceMembership>> {
    let rows: Vec<MembershipRow> = sqlx::query_as(
        "SELECT w.id, w.name, w.created_by, w.created_at, w.updated_at, m.role
         FROM workspaces w
         JOIN workspace_members m ON m.workspace_id = w.id
         WHERE m.user_id = $1
         ORDER BY w.created_at",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| WorkspaceMembership {
            workspace: r.workspace,
            role: r.role,
        })
        .collect())
}

/// Create a workspace; the creator becomes its `owner`.
pub async fn create(pool: &ControlPool, name: &str, creator: Uuid) -> AppResult<Workspace> {
    let mut tx = pool.begin().await?;
    let workspace: Workspace =
        sqlx::query_as("INSERT INTO workspaces (name, created_by) VALUES ($1, $2) RETURNING *")
            .bind(name)
            .bind(creator)
            .fetch_one(&mut *tx)
            .await?;
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, 'owner')",
    )
    .bind(workspace.id)
    .bind(creator)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(workspace)
}

pub async fn fetch(pool: &ControlPool, id: Uuid) -> AppResult<Workspace> {
    sqlx::query_as::<_, Workspace>("SELECT * FROM workspaces WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or(AppError::NotFound("workspace"))
}

/// `fetch` by name, for idempotent seeding.
pub async fn find_by_name(pool: &ControlPool, name: &str) -> AppResult<Option<Workspace>> {
    Ok(
        sqlx::query_as::<_, Workspace>("SELECT * FROM workspaces WHERE name = $1")
            .bind(name)
            .fetch_optional(pool)
            .await?,
    )
}

/// A member row, with the profile fields the members UI needs -- see
/// `GET /workspaces/{id}/members`.
#[derive(Debug, Clone, Serialize, FromRow, ToSchema)]
pub struct Member {
    pub user_id: Uuid,
    pub email: String,
    pub display_name: Option<String>,
    pub role: Role,
    pub created_at: DateTime<Utc>,
}

/// Every member of a workspace, oldest first.
pub async fn list_members(pool: &ControlPool, workspace_id: Uuid) -> AppResult<Vec<Member>> {
    Ok(sqlx::query_as(
        "SELECT m.user_id, p.email, p.display_name, m.role, m.created_at
         FROM workspace_members m
         JOIN profiles p ON p.id = m.user_id
         WHERE m.workspace_id = $1
         ORDER BY m.created_at",
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await?)
}

/// Whether `email` (matched case-insensitively, the same way invite
/// acceptance matches it) already belongs to a member of this workspace --
/// so invite creation can refuse with 409 rather than let acceptance later
/// collide with an existing membership (see `crate::control::invites`).
pub async fn member_with_email_exists(
    pool: &ControlPool,
    workspace_id: Uuid,
    email: &str,
) -> AppResult<bool> {
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
            SELECT 1 FROM workspace_members m
            JOIN profiles p ON p.id = m.user_id
            WHERE m.workspace_id = $1 AND lower(p.email) = lower($2)
        )",
    )
    .bind(workspace_id)
    .bind(email)
    .fetch_one(pool)
    .await?)
}

/// How many OTHER owners this workspace has, for the last-owner guard: a
/// change to `exclude_user`'s own membership (demote, remove, or a
/// transfer's demotion of the outgoing owner) is safe only if at least one
/// owner remains once this user's current role no longer counts.
///
/// Reads outside any lock -- fine for display/advisory use, but NOT the
/// last-owner guard itself (see `locked_owner_count_excluding`): calling
/// this, then separately writing the role change, is exactly the
/// check-then-act gap that let two concurrent demotions each observe "the
/// other owner" and both proceed, leaving zero.
pub async fn owner_count_excluding(
    pool: &ControlPool,
    workspace_id: Uuid,
    exclude_user: Uuid,
) -> AppResult<i64> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM workspace_members
         WHERE workspace_id = $1 AND role = 'owner' AND user_id <> $2",
    )
    .bind(workspace_id)
    .bind(exclude_user)
    .fetch_one(pool)
    .await?)
}

/// Insert or update a member's role with no authorization or last-owner
/// check at all -- idempotent, for invite acceptance (which only ever
/// grants a NEW member a role, never demotes an existing one; see
/// `crate::control::invites`) and test fixtures. `PATCH
/// /workspaces/{id}/members/{user_id}` must go through [`change_role`]
/// instead, which runs `Role::can_assign` and the last-owner guard
/// atomically.
pub async fn set_role(
    pool: &ControlPool,
    workspace_id: Uuid,
    user_id: Uuid,
    role: Role,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, $3)
         ON CONFLICT (workspace_id, user_id) DO UPDATE SET role = $3",
    )
    .bind(workspace_id)
    .bind(user_id)
    .bind(role)
    .execute(pool)
    .await?;
    Ok(())
}

/// Lock every member row of `workspace_id` for the rest of `tx`
/// (`SELECT ... FOR UPDATE`): the last-owner guard is a count-then-act
/// check, so without a lock two concurrent calls on the same workspace
/// (two demotions, a demotion racing a removal, a removal racing a
/// transfer, ...) could each read "another owner remains" before either
/// writes, and both proceed. A second transaction's `lock_members` call
/// blocks here until the first commits or rolls back, then its own reads
/// see the up-to-date membership. A workspace's membership is small
/// (people, not rows-at-scale), so locking all of it is cheap and simplest
/// to reason about -- no per-row lock ordering to get wrong.
async fn lock_members(tx: &mut sqlx::PgConnection, workspace_id: Uuid) -> AppResult<()> {
    sqlx::query("SELECT 1 FROM workspace_members WHERE workspace_id = $1 FOR UPDATE")
        .bind(workspace_id)
        .fetch_all(tx)
        .await?;
    Ok(())
}

/// `role_for`, but reading through `tx` so it sees `lock_members`'
/// consistent, locked snapshot rather than a fresh unlocked read.
async fn locked_role_for(
    tx: &mut sqlx::PgConnection,
    workspace_id: Uuid,
    user_id: Uuid,
) -> AppResult<Option<Role>> {
    Ok(sqlx::query_scalar::<_, Role>(
        "SELECT role FROM workspace_members WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(workspace_id)
    .bind(user_id)
    .fetch_optional(tx)
    .await?)
}

/// `owner_count_excluding`, but reading through `tx` (see `locked_role_for`).
async fn locked_owner_count_excluding(
    tx: &mut sqlx::PgConnection,
    workspace_id: Uuid,
    exclude_user: Uuid,
) -> AppResult<i64> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM workspace_members
         WHERE workspace_id = $1 AND role = 'owner' AND user_id <> $2",
    )
    .bind(workspace_id)
    .bind(exclude_user)
    .fetch_one(tx)
    .await?)
}

const NO_OWNER_LEFT: &str = "the workspace must keep at least one owner";

/// Atomically authorize and apply a role change for an existing member:
/// `Role::can_assign` and the last-owner guard both run inside the same
/// locked transaction as the write (see `lock_members`), so this can't
/// race another call changing/removing a member of the same workspace.
/// 404 if `user_id` isn't a member; 403 if `actor_role` isn't allowed to
/// make this particular change; 409 if it would leave no owner.
pub async fn change_role(
    pool: &ControlPool,
    workspace_id: Uuid,
    actor_role: Role,
    user_id: Uuid,
    new_role: Role,
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    lock_members(&mut tx, workspace_id).await?;
    let current = locked_role_for(&mut tx, workspace_id, user_id)
        .await?
        .ok_or(AppError::NotFound("member"))?;
    if !Role::can_assign(actor_role, Some(current), Some(new_role)) {
        return Err(AppError::Forbidden(format!(
            "the {} role cannot change a {} member to {}",
            actor_role.as_str(),
            current.as_str(),
            new_role.as_str()
        )));
    }
    if current == Role::Owner
        && new_role != Role::Owner
        && locked_owner_count_excluding(&mut tx, workspace_id, user_id).await? == 0
    {
        return Err(AppError::Conflict(NO_OWNER_LEFT.to_owned()));
    }
    sqlx::query("UPDATE workspace_members SET role = $3 WHERE workspace_id = $1 AND user_id = $2")
        .bind(workspace_id)
        .bind(user_id)
        .bind(new_role)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Atomically authorize and apply removing an existing member -- same
/// shape and same lock as [`change_role`]. 404 if `user_id` isn't a
/// member; 403 if `actor_role` isn't allowed to remove them; 409 if it
/// would leave no owner.
pub async fn remove_member(
    pool: &ControlPool,
    workspace_id: Uuid,
    actor_role: Role,
    user_id: Uuid,
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    lock_members(&mut tx, workspace_id).await?;
    let current = locked_role_for(&mut tx, workspace_id, user_id)
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
        && locked_owner_count_excluding(&mut tx, workspace_id, user_id).await? == 0
    {
        return Err(AppError::Conflict(NO_OWNER_LEFT.to_owned()));
    }
    sqlx::query("DELETE FROM workspace_members WHERE workspace_id = $1 AND user_id = $2")
        .bind(workspace_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Move ownership from `from_user` to `to_user` (already a member) in one
/// locked transaction (see `lock_members`), so the workspace is never
/// observed without an owner between the two writes, and so a concurrent
/// change to either user's membership on the same workspace can't run
/// between this transaction's reads and its writes. 403 if `from_user` is
/// not (still, as of the lock) an owner; 404 if `to_user` is not a member.
pub async fn transfer_ownership(
    pool: &ControlPool,
    workspace_id: Uuid,
    from_user: Uuid,
    to_user: Uuid,
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    lock_members(&mut tx, workspace_id).await?;
    let from_role = locked_role_for(&mut tx, workspace_id, from_user)
        .await?
        .ok_or(AppError::NotFound("member"))?;
    if from_role != Role::Owner {
        return Err(AppError::Forbidden(
            "transferring ownership requires the owner role".to_owned(),
        ));
    }
    locked_role_for(&mut tx, workspace_id, to_user)
        .await?
        .ok_or(AppError::NotFound("member"))?;
    sqlx::query(
        "UPDATE workspace_members SET role = 'admin' WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(workspace_id)
    .bind(from_user)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE workspace_members SET role = 'owner' WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(workspace_id)
    .bind(to_user)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}
