//! One row per Supabase Auth user, keyed by their auth user id.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use super::ControlPool;
use crate::error::AppResult;

#[derive(Debug, Clone, Serialize, FromRow, ToSchema)]
pub struct Profile {
    pub id: Uuid,
    pub email: String,
    pub display_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Ensure a profile exists for a verified JWT's `(id, email)`, refreshing
/// the email if it changed. Also best-effort upserts the local `auth.users`
/// shim so the `profiles` FK is satisfiable; a real Supabase project
/// already has that row (managed by GoTrue) and the backend's role may not
/// have write access to it, so a failure there is swallowed.
pub async fn upsert_from_jwt(pool: &ControlPool, id: Uuid, email: &str) -> AppResult<Profile> {
    let _ = sqlx::query(
        "INSERT INTO auth.users (id, email) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING",
    )
    .bind(id)
    .bind(email)
    .execute(pool)
    .await;
    Ok(sqlx::query_as::<_, Profile>(
        "INSERT INTO profiles (id, email) VALUES ($1, $2)
         ON CONFLICT (id) DO UPDATE SET email = $2, updated_at = now()
         RETURNING id, email, display_name, created_at, updated_at",
    )
    .bind(id)
    .bind(email)
    .fetch_one(pool)
    .await?)
}
