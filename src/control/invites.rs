//! `workspace_invites`: phase 1 of B7. The operator creates the auth user
//! from the Supabase dashboard (there is no `IdentityAdmin::invite` yet --
//! see plan B8); this table is what lets the person land in the right
//! workspace, with the right role, the first time they're seen here.
//!
//! Matching is on the VERIFIED email claim only (`crate::auth::provision`
//! calls [`accept_pending_for_email`] with the email out of a verified
//! JWT, never a client-supplied one), and only against invites that are
//! still unaccepted and unexpired -- an accepted or expired row is inert.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use super::ControlPool;
use super::workspaces::Role;
use crate::error::AppResult;

/// How long a freshly-created invite stays claimable.
const INVITE_TTL_DAYS: i64 = 14;

#[derive(Debug, Clone, Serialize, FromRow, ToSchema)]
pub struct Invite {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub email: String,
    pub role: Role,
    pub invited_by: Option<Uuid>,
    pub expires_at: DateTime<Utc>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewInvite {
    pub email: String,
    pub role: Role,
}

/// Create a pending invite. Fails with a unique-violation (the caller maps
/// it to 409) if this workspace already has one pending for this email.
pub async fn create(
    pool: &ControlPool,
    workspace_id: Uuid,
    invited_by: Uuid,
    email: &str,
    role: Role,
) -> Result<Invite, sqlx::Error> {
    sqlx::query_as(
        "INSERT INTO workspace_invites (workspace_id, email, role, invited_by, expires_at)
         VALUES ($1, $2, $3, $4, now() + ($5 || ' days')::interval)
         RETURNING *",
    )
    .bind(workspace_id)
    .bind(email)
    .bind(role)
    .bind(invited_by)
    .bind(INVITE_TTL_DAYS.to_string())
    .fetch_one(pool)
    .await
}

/// Every still-pending (unaccepted) invite for a workspace, newest first.
pub async fn list_pending(pool: &ControlPool, workspace_id: Uuid) -> AppResult<Vec<Invite>> {
    Ok(sqlx::query_as(
        "SELECT * FROM workspace_invites
         WHERE workspace_id = $1 AND accepted_at IS NULL
         ORDER BY created_at DESC",
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await?)
}

/// A single invite, scoped to the workspace it was made in -- so a caller
/// cannot act on another workspace's invite id by guessing it.
pub async fn fetch(pool: &ControlPool, workspace_id: Uuid, id: Uuid) -> AppResult<Option<Invite>> {
    Ok(
        sqlx::query_as("SELECT * FROM workspace_invites WHERE id = $1 AND workspace_id = $2")
            .bind(id)
            .bind(workspace_id)
            .fetch_optional(pool)
            .await?,
    )
}

pub async fn delete(pool: &ControlPool, workspace_id: Uuid, id: Uuid) -> AppResult<()> {
    sqlx::query("DELETE FROM workspace_invites WHERE id = $1 AND workspace_id = $2")
        .bind(id)
        .bind(workspace_id)
        .execute(pool)
        .await?;
    Ok(())
}

#[derive(FromRow)]
struct PendingMatch {
    id: Uuid,
    workspace_id: Uuid,
    role: Role,
}

/// Claim every still-pending, unexpired invite for `email` on behalf of
/// `user_id`: grant the invited role in each workspace and mark the invite
/// accepted. Idempotent (an already-accepted invite is never matched
/// again) and best-effort from the caller's point of view -- see
/// `crate::auth::provision`.
pub async fn accept_pending_for_email(
    pool: &ControlPool,
    user_id: Uuid,
    email: &str,
) -> AppResult<()> {
    let pending: Vec<PendingMatch> = sqlx::query_as(
        "SELECT id, workspace_id, role FROM workspace_invites
         WHERE lower(email) = lower($1) AND accepted_at IS NULL AND expires_at > now()",
    )
    .bind(email)
    .fetch_all(pool)
    .await?;

    for invite in pending {
        let mut tx = pool.begin().await?;
        sqlx::query(
            "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, $3)
             ON CONFLICT (workspace_id, user_id) DO UPDATE SET role = $3",
        )
        .bind(invite.workspace_id)
        .bind(user_id)
        .bind(invite.role)
        .execute(&mut *tx)
        .await?;
        // `accepted_at IS NULL` in the WHERE guards against two concurrent
        // requests from the same newly-provisioned user both claiming the
        // same invite; only one UPDATE actually touches a row.
        sqlx::query(
            "UPDATE workspace_invites SET accepted_at = now()
             WHERE id = $1 AND accepted_at IS NULL",
        )
        .bind(invite.id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
    }
    Ok(())
}
