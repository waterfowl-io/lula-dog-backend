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
///   of `viewer`/`member`/`admin` members -- see `can_assign`) + delete any
///   workspace-level resource or member (invites, members; see
///   `crate::members` and `docs/architecture.md`'s DELETE-route table).
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
    /// publish/discard/rollback, all `member`+), `members::require_admin`
    /// (`admin`+) and the chat build-mode gate alike -- so none of them can
    /// quietly compare roles differently from the others.
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

/// How many OTHER owners this workspace has, for the last-owner guard: a
/// change to `exclude_user`'s own membership (demote, remove, or a
/// transfer's demotion of the outgoing owner) is safe only if at least one
/// owner remains once this user's current role no longer counts.
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

/// Insert or update a member's role. Idempotent; used both by invite
/// acceptance and by `PATCH /workspaces/{id}/members/{user_id}`.
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

pub async fn remove_member(pool: &ControlPool, workspace_id: Uuid, user_id: Uuid) -> AppResult<()> {
    sqlx::query("DELETE FROM workspace_members WHERE workspace_id = $1 AND user_id = $2")
        .bind(workspace_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Move ownership from `from_user` to `to_user` (already a member) in one
/// transaction, so the workspace is never observed without an owner
/// between the two writes.
pub async fn transfer_ownership(
    pool: &ControlPool,
    workspace_id: Uuid,
    from_user: Uuid,
    to_user: Uuid,
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
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
