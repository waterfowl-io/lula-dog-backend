//! Request extractors whose rejections are `AppError`s, so a bad body or path
//! gets the standard JSON error shape instead of axum's plain-text one.

use axum::extract::{FromRequest, FromRequestParts};
use serde::Deserialize;

use crate::error::AppError;

#[derive(FromRequest)]
#[from_request(via(axum::Json), rejection(AppError))]
pub struct ApiJson<T>(pub T);

#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(AppError))]
pub struct ApiPath<T>(pub T);

#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Query), rejection(AppError))]
pub struct ApiQuery<T>(pub T);

/// Just the `{id}` segment of a route's path, by field name. Every
/// data-plane route now lives under `/w/{workspace_id}/...`, so a bare
/// `ApiPath<i64>` would see two captured segments instead of one; a
/// named-field struct matches by name and ignores the rest.
#[derive(Debug, Deserialize)]
pub struct IdPath {
    pub id: i64,
}
