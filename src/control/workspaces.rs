//! Workspaces and membership. A workspace's editable data lives in its own
//! SQLite file (`crate::workspace_db`), not here.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use super::ControlPool;
use crate::error::{AppError, AppResult};

/// `owner` and `admin` manage membership and settings; `member` and
/// `viewer` cannot. `viewer` cannot write workspace data; the other three
/// can. Ordered least to most privileged.
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
    pub const fn can_write(self) -> bool {
        !matches!(self, Self::Viewer)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Member => "member",
            Self::Viewer => "viewer",
        }
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
