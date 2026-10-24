//! Google Places Text Search (New): a reqwest/rustls client against
//! `places.googleapis.com`, same shape as `GrokExtractor`.

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::{PlaceQuery, PlaceResult, PlacesClient, PlacesError};

const TIMEOUT: Duration = Duration::from_secs(20);
const ERROR_BODY_MAX_CHARS: usize = 500;
const FIELD_MASK: &str = "places.websiteUri,places.internationalPhoneNumber,places.primaryType";

pub struct GooglePlacesClient {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl GooglePlacesClient {
    /// `base_url` is the API root, e.g. `https://places.googleapis.com/v1`.
    pub fn new(api_key: String, base_url: &str) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder().timeout(TIMEOUT).build()?;
        Ok(Self {
            client,
            api_key,
            base_url: base_url.trim_end_matches('/').to_owned(),
        })
    }
}

#[derive(Deserialize, Default)]
struct SearchResponse {
    #[serde(default)]
    places: Vec<Place>,
}

#[derive(Deserialize)]
struct Place {
    #[serde(rename = "websiteUri")]
    website_uri: Option<String>,
    #[serde(rename = "internationalPhoneNumber")]
    international_phone_number: Option<String>,
    #[serde(rename = "primaryType")]
    primary_type: Option<String>,
}

#[async_trait]
impl PlacesClient for GooglePlacesClient {
    async fn search(&self, query: PlaceQuery<'_>) -> Result<Option<PlaceResult>, PlacesError> {
        let text_query = query.address.map_or_else(
            || query.name.to_owned(),
            |addr| format!("{} {addr}", query.name),
        );
        let response = self
            .client
            .post(format!("{}/places:searchText", self.base_url))
            .header("X-Goog-Api-Key", &self.api_key)
            .header("X-Goog-FieldMask", FIELD_MASK)
            .json(&json!({"textQuery": text_query}))
            .send()
            .await
            .map_err(|e| PlacesError::Request(e.without_url().to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(PlacesError::Status {
                status: status.as_u16(),
                body: body.chars().take(ERROR_BODY_MAX_CHARS).collect(),
            });
        }

        let parsed: SearchResponse = response
            .json()
            .await
            .map_err(|e| PlacesError::Malformed(e.without_url().to_string()))?;
        Ok(parsed.places.into_iter().next().map(|p| PlaceResult {
            website: p.website_uri,
            phone: p.international_phone_number,
            category: p.primary_type,
        }))
    }
}
