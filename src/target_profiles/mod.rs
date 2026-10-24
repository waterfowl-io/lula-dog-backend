//! Target profiles: Jack's saved search (industries, regions, size,
//! include/exclude keywords, minimum fit score). CRUD only; profiles are
//! read by the cheap filter, the classifier and lead queueing.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sqlx::types::Json as SqlxJson;
use sqlx::{FromRow, SqliteConnection};
use utoipa::ToSchema;

use crate::AppState;
use crate::error::{AppError, AppResult};
use crate::extract::{ApiJson, ApiPath};
use crate::opportunities::{not_null, nullable};

#[derive(Debug, Clone, Serialize, FromRow, ToSchema)]
pub struct TargetProfile {
    pub id: i64,
    pub name: String,
    #[schema(value_type = Vec<String>)]
    pub industries: SqlxJson<Vec<String>>,
    #[schema(value_type = Vec<String>)]
    pub regions: SqlxJson<Vec<String>>,
    pub size_min: Option<i64>,
    pub size_max: Option<i64>,
    pub size_notes: Option<String>,
    #[schema(value_type = Vec<String>)]
    pub include_keywords: SqlxJson<Vec<String>>,
    #[schema(value_type = Vec<String>)]
    pub exclude_keywords: SqlxJson<Vec<String>>,
    pub min_fit_score: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewTargetProfile {
    pub name: String,
    #[serde(default)]
    pub industries: Vec<String>,
    #[serde(default)]
    pub regions: Vec<String>,
    pub size_min: Option<i64>,
    pub size_max: Option<i64>,
    pub size_notes: Option<String>,
    #[serde(default)]
    pub include_keywords: Vec<String>,
    #[serde(default)]
    pub exclude_keywords: Vec<String>,
    pub min_fit_score: Option<i64>,
}

fn name(raw: &str) -> AppResult<String> {
    let t = raw.trim();
    if t.is_empty() {
        return Err(AppError::BadRequest("name cannot be empty".to_owned()));
    }
    Ok(t.to_owned())
}

/// Validate and insert. The one writer of new target profile rows; the seed
/// command uses it too.
pub async fn insert(
    conn: &mut SqliteConnection,
    new: NewTargetProfile,
) -> AppResult<TargetProfile> {
    Ok(sqlx::query_as::<_, TargetProfile>(
        "INSERT INTO target_profiles
           (name, industries, regions, size_min, size_max, size_notes,
            include_keywords, exclude_keywords, min_fit_score)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         RETURNING *",
    )
    .bind(name(&new.name)?)
    .bind(SqlxJson(new.industries))
    .bind(SqlxJson(new.regions))
    .bind(new.size_min)
    .bind(new.size_max)
    .bind(new.size_notes)
    .bind(SqlxJson(new.include_keywords))
    .bind(SqlxJson(new.exclude_keywords))
    .bind(new.min_fit_score.unwrap_or(50))
    .fetch_one(conn)
    .await?)
}

pub async fn fetch(conn: &mut SqliteConnection, id: i64) -> AppResult<TargetProfile> {
    sqlx::query_as::<_, TargetProfile>("SELECT * FROM target_profiles WHERE id = ?1")
        .bind(id)
        .fetch_optional(conn)
        .await?
        .ok_or(AppError::NotFound("target profile"))
}

pub async fn list(conn: &mut SqliteConnection) -> AppResult<Vec<TargetProfile>> {
    Ok(
        sqlx::query_as::<_, TargetProfile>("SELECT * FROM target_profiles ORDER BY id")
            .fetch_all(conn)
            .await?,
    )
}

/// List profiles. Created most recently use `GET /target-profiles/{id}` for one.
#[utoipa::path(
    get,
    path = "/target-profiles",
    responses((status = 200, body = [TargetProfile])),
    tag = "target-profiles"
)]
pub async fn list_handler(State(state): State<AppState>) -> AppResult<Json<Vec<TargetProfile>>> {
    let mut conn = state.pool.acquire().await?;
    Ok(Json(list(&mut conn).await?))
}

/// Create a target profile.
#[utoipa::path(
    post,
    path = "/target-profiles",
    request_body = NewTargetProfile,
    responses(
        (status = 201, body = TargetProfile),
        (status = 400, body = crate::error::ErrorBody),
    ),
    tag = "target-profiles"
)]
pub async fn create(
    State(state): State<AppState>,
    ApiJson(new): ApiJson<NewTargetProfile>,
) -> AppResult<(StatusCode, Json<TargetProfile>)> {
    let mut conn = state.pool.acquire().await?;
    Ok((StatusCode::CREATED, Json(insert(&mut conn, new).await?)))
}

/// Fetch one target profile.
#[utoipa::path(
    get,
    path = "/target-profiles/{id}",
    params(("id" = i64, Path)),
    responses(
        (status = 200, body = TargetProfile),
        (status = 404, body = crate::error::ErrorBody),
    ),
    tag = "target-profiles"
)]
pub async fn get(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<i64>,
) -> AppResult<Json<TargetProfile>> {
    let mut conn = state.pool.acquire().await?;
    Ok(Json(fetch(&mut conn, id).await?))
}

/// A partial update. An absent field is left alone; `null` clears a nullable
/// field and is rejected for `name`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TargetProfilePatch {
    #[serde(default, deserialize_with = "not_null")]
    pub name: Option<String>,
    pub industries: Option<Vec<String>>,
    pub regions: Option<Vec<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<i64>)]
    pub size_min: Option<Option<i64>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<i64>)]
    pub size_max: Option<Option<i64>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub size_notes: Option<Option<String>>,
    pub include_keywords: Option<Vec<String>>,
    pub exclude_keywords: Option<Vec<String>>,
    #[serde(default, deserialize_with = "not_null")]
    pub min_fit_score: Option<i64>,
}

/// Edit a target profile.
#[utoipa::path(
    patch,
    path = "/target-profiles/{id}",
    params(("id" = i64, Path)),
    request_body = TargetProfilePatch,
    responses(
        (status = 200, body = TargetProfile),
        (status = 400, body = crate::error::ErrorBody),
        (status = 404, body = crate::error::ErrorBody),
    ),
    tag = "target-profiles"
)]
pub async fn patch(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<i64>,
    ApiJson(patch): ApiJson<TargetProfilePatch>,
) -> AppResult<Json<TargetProfile>> {
    let mut conn = state.pool.acquire().await?;
    let current = fetch(&mut conn, id).await?;
    let name = match patch.name {
        Some(n) => name(&n)?,
        None => current.name,
    };
    let updated = sqlx::query_as::<_, TargetProfile>(
        "UPDATE target_profiles SET
           name = ?1, industries = ?2, regions = ?3, size_min = ?4, size_max = ?5,
           size_notes = ?6, include_keywords = ?7, exclude_keywords = ?8, min_fit_score = ?9,
           updated_at = CURRENT_TIMESTAMP
         WHERE id = ?10
         RETURNING *",
    )
    .bind(name)
    .bind(SqlxJson(patch.industries.unwrap_or(current.industries.0)))
    .bind(SqlxJson(patch.regions.unwrap_or(current.regions.0)))
    .bind(patch.size_min.unwrap_or(current.size_min))
    .bind(patch.size_max.unwrap_or(current.size_max))
    .bind(patch.size_notes.unwrap_or(current.size_notes))
    .bind(SqlxJson(
        patch.include_keywords.unwrap_or(current.include_keywords.0),
    ))
    .bind(SqlxJson(
        patch.exclude_keywords.unwrap_or(current.exclude_keywords.0),
    ))
    .bind(patch.min_fit_score.unwrap_or(current.min_fit_score))
    .bind(id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(Json(updated))
}

/// Delete a target profile.
#[utoipa::path(
    delete,
    path = "/target-profiles/{id}",
    params(("id" = i64, Path)),
    responses(
        (status = 204),
        (status = 404, body = crate::error::ErrorBody),
    ),
    tag = "target-profiles"
)]
pub async fn delete(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<i64>,
) -> AppResult<StatusCode> {
    let mut conn = state.pool.acquire().await?;
    let deleted = sqlx::query("DELETE FROM target_profiles WHERE id = ?1")
        .bind(id)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    if deleted == 0 {
        return Err(AppError::NotFound("target profile"));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Jack: packaging buyers in Illinois. Inserted only if the table is empty,
/// so `cargo run -- seed` (or calling this again) is safe to repeat.
pub async fn seed_default(conn: &mut SqliteConnection, min_fit_score: i64) -> AppResult<()> {
    let existing: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM target_profiles")
        .fetch_one(&mut *conn)
        .await?;
    if existing > 0 {
        return Ok(());
    }
    insert(
        conn,
        NewTargetProfile {
            name: "Jack: Illinois packaging buyers".to_owned(),
            industries: [
                "foods",
                "brewing",
                "bakery",
                "cosmetics",
                "candle",
                "coffee",
                "supply",
                "distribution",
                "manufacturing",
                "e-commerce",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            regions: vec!["IL".to_owned()],
            size_min: None,
            size_max: None,
            size_notes: Some(
                "Small to mid-size operating businesses that ship physical goods".to_owned(),
            ),
            include_keywords: [
                "foods",
                "brewing",
                "bakery",
                "cosmetics",
                "candle",
                "coffee",
                "supply",
                "distribution",
                "manufacturing",
                "e-commerce",
                "packaging",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            exclude_keywords: [
                "law firm",
                "insurance",
                "bank",
                "church",
                "nonprofit",
                "government",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            min_fit_score: Some(min_fit_score),
        },
    )
    .await?;
    Ok(())
}
