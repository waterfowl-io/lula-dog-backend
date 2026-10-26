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
/// the email if it changed. `profiles.id` carries no foreign key (see
/// `migrations/control/0001_control_plane.sql`): this service verifies
/// identity itself and connects to Postgres directly, so there is no
/// Supabase-managed `auth.users` row for it to reference.
pub async fn upsert_from_jwt(pool: &ControlPool, id: Uuid, email: &str) -> AppResult<Profile> {
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
