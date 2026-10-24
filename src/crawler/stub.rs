//! A deterministic, network-free `Fetcher`. Every URL gets the same canned
//! page with about/products/contact links, so `crawl` has something to walk
//! offline; not a substitute for a real fetch.

use async_trait::async_trait;

use super::{FetchError, FetchedPage, Fetcher};

const PAGE: &str = "<html><body>\
    <h1>Welcome</h1><p>We make great packaging products and ship nationwide.</p>\
    <a href=\"/about\">About</a> <a href=\"/products\">Products</a> \
    <a href=\"/contact\">Contact</a> <a href=\"https://other.example/x\">Other site</a>\
    </body></html>";

#[derive(Debug, Default, Clone, Copy)]
pub struct StubFetcher;

#[async_trait]
impl Fetcher for StubFetcher {
    async fn fetch(&self, url: &str) -> Result<FetchedPage, FetchError> {
        if url.ends_with("/robots.txt") {
            return Ok(FetchedPage {
                status: 404,
                body: String::new(),
            });
        }
        Ok(FetchedPage {
            status: 200,
            body: PAGE.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn serves_the_same_page_for_any_url() -> Result<(), FetchError> {
        let fetcher = StubFetcher;
        let a = fetcher.fetch("https://acme.example").await?;
        let b = fetcher.fetch("https://acme.example/about").await?;
        assert_eq!(a.status, 200);
        assert_eq!(a.body, b.body);
        Ok(())
    }

    #[tokio::test]
    async fn has_no_robots_restrictions() -> Result<(), FetchError> {
        let fetcher = StubFetcher;
        let robots = fetcher.fetch("https://acme.example/robots.txt").await?;
        assert_eq!(robots.status, 404);
        Ok(())
    }
}
