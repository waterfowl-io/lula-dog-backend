//! A deterministic, network-free `PlacesClient`. Builds a plausible
//! `.example` website from the company name so the rest of the pipeline
//! (crawl, classify) has something to work with offline; not a substitute
//! for a real lookup.

use async_trait::async_trait;

use super::{PlaceQuery, PlaceResult, PlacesClient, PlacesError};
use crate::companies::normalize_name;

#[derive(Debug, Default, Clone, Copy)]
pub struct StubPlacesClient;

#[async_trait]
impl PlacesClient for StubPlacesClient {
    async fn search(&self, query: PlaceQuery<'_>) -> Result<Option<PlaceResult>, PlacesError> {
        let slug = normalize_name(query.name).replace(' ', "-");
        if slug.is_empty() {
            return Ok(None);
        }
        Ok(Some(PlaceResult {
            website: Some(format!("https://{slug}.example.com")),
            phone: None,
            category: None,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn builds_a_deterministic_example_website() -> Result<(), PlacesError> {
        let client = StubPlacesClient;
        let got = client
            .search(PlaceQuery {
                name: "Acme Coffee Roasters, LLC",
                address: None,
            })
            .await?;
        assert_eq!(
            got,
            Some(PlaceResult {
                website: Some("https://acme-coffee-roasters-llc.example.com".to_owned()),
                phone: None,
                category: None,
            })
        );
        Ok(())
    }

    #[tokio::test]
    async fn is_deterministic() -> Result<(), PlacesError> {
        let client = StubPlacesClient;
        let a = client
            .search(PlaceQuery {
                name: "Bluebird Apparel",
                address: Some("100 Main St"),
            })
            .await?;
        let b = client
            .search(PlaceQuery {
                name: "Bluebird Apparel",
                address: Some("100 Main St"),
            })
            .await?;
        assert_eq!(a, b);
        Ok(())
    }
}
