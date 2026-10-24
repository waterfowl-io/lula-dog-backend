//! The real `Fetcher`: reqwest/rustls, a polite user agent, a per-request
//! timeout and a response size cap.
//!
//! The size cap is enforced two ways: a response whose `Content-Length`
//! already exceeds the cap is rejected before its body is read; a response
//! with no (or an understated) `Content-Length` is still downloaded in
//! full and then truncated. The first case is the common one for real
//! sites; the second is a known gap for adversarial or misreporting
//! servers, documented here rather than solved with a streaming reader to
//! keep this dependency-free.

use std::time::Duration;

use async_trait::async_trait;

use super::{FetchError, FetchedPage, Fetcher};

const USER_AGENT: &str = "lula-dog-leadgen-bot/0.1 (+https://lula.dog; contact: hello@lula.dog)";

pub struct HttpFetcher {
    client: reqwest::Client,
    max_bytes: u64,
}

impl HttpFetcher {
    pub fn new(timeout: Duration, max_bytes: u64) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .user_agent(USER_AGENT)
            .build()?;
        Ok(Self { client, max_bytes })
    }
}

#[async_trait]
impl Fetcher for HttpFetcher {
    async fn fetch(&self, url: &str) -> Result<FetchedPage, FetchError> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| FetchError::Request(e.without_url().to_string()))?;
        let status = response.status().as_u16();
        if response
            .content_length()
            .is_some_and(|len| len > self.max_bytes)
        {
            return Ok(FetchedPage {
                status,
                body: String::new(),
            });
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|e| FetchError::Request(e.without_url().to_string()))?;
        let max_bytes = usize::try_from(self.max_bytes).unwrap_or(usize::MAX);
        let capped = bytes.get(..max_bytes.min(bytes.len())).unwrap_or(&bytes);
        Ok(FetchedPage {
            status,
            body: String::from_utf8_lossy(capped).into_owned(),
        })
    }
}
