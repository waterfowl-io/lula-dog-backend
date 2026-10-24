//! The website-lookup seam, same shape as `crate::extractor`: given a
//! company name and address, a `PlacesClient` returns what it found
//! (website, phone, category). Two impls: `StubPlacesClient` (deterministic,
//! the default) and `GooglePlacesClient` (Google Places Text Search, New).

mod google;
mod stub;

use async_trait::async_trait;

pub use google::GooglePlacesClient;
pub use stub::StubPlacesClient;

pub struct PlaceQuery<'a> {
    pub name: &'a str,
    pub address: Option<&'a str>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PlaceResult {
    pub website: Option<String>,
    pub phone: Option<String>,
    pub category: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum PlacesError {
    #[error("places request failed: {0}")]
    Request(String),
    #[error("places returned HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error("places returned malformed output: {0}")]
    Malformed(String),
}

#[async_trait]
pub trait PlacesClient: Send + Sync {
    /// `None` when nothing matched the query; a hard failure is `Err`.
    async fn search(&self, query: PlaceQuery<'_>) -> Result<Option<PlaceResult>, PlacesError>;
}
