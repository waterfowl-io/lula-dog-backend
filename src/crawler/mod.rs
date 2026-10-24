//! A small, polite crawler: home page plus a few same-domain links that look
//! like about/products/contact, text only. Respects `robots.txt`, a
//! per-request timeout, a response size cap and a page-count cap.
//!
//! The network boundary is the `Fetcher` trait (`StubFetcher`, the default,
//! and `HttpFetcher`), the same seam as `crate::extractor`: tests drive
//! `crawl` against a loopback fake server, never the real network.

mod html;
mod http;
mod robots;
mod stub;

use async_trait::async_trait;

pub use crate::config::CrawlLimits;
pub use http::HttpFetcher;
pub use stub::StubFetcher;

#[derive(Debug, Clone)]
pub struct FetchedPage {
    pub status: u16,
    pub body: String,
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("request failed: {0}")]
    Request(String),
}

#[async_trait]
pub trait Fetcher: Send + Sync {
    async fn fetch(&self, url: &str) -> Result<FetchedPage, FetchError>;
}

#[derive(Debug, Clone)]
pub struct CrawledPage {
    pub url: String,
    pub text: String,
}

#[derive(Debug, thiserror::Error)]
#[error("fetching {url}: {source}")]
pub struct CrawlError {
    pub url: String,
    #[source]
    pub source: FetchError,
}

/// Crawl `website`'s home page plus up to `limits.max_pages - 1` same-domain
/// links that look like about/products/contact. `website` may be a bare
/// domain or a full URL; `https://` is assumed when no scheme is given.
/// Returns an empty list (not an error) when `robots.txt` disallows the
/// home page or the site has no host at all; a genuine fetch failure on the
/// home page is `Err`.
pub async fn crawl(
    fetcher: &dyn Fetcher,
    website: &str,
    limits: CrawlLimits,
) -> Result<Vec<CrawledPage>, CrawlError> {
    let Some(domain) = crate::companies::normalize_domain(website) else {
        return Ok(Vec::new());
    };
    let base = if website.contains("://") {
        website.trim_end_matches('/').to_owned()
    } else {
        format!("https://{domain}")
    };

    let disallow = match fetcher.fetch(&format!("{base}/robots.txt")).await {
        Ok(page) if page.status == 200 => robots::parse(&page.body),
        _ => Vec::new(),
    };
    if !robots::allowed("/", &disallow) {
        return Ok(Vec::new());
    }

    let home = fetcher.fetch(&base).await.map_err(|source| CrawlError {
        url: base.clone(),
        source,
    })?;
    let mut pages = Vec::new();
    let mut links = Vec::new();
    if home.status == 200 {
        links = html::extract_links(&home.body);
        pages.push(CrawledPage {
            url: base.clone(),
            text: html::strip_tags(&home.body),
        });
    }

    let mut visited = std::collections::HashSet::new();
    visited.insert(base.clone());
    let max_extra = usize::try_from(limits.max_pages.saturating_sub(1)).unwrap_or(usize::MAX);
    let mut extra_count = 0usize;
    for href in links {
        if extra_count >= max_extra {
            break;
        }
        let Some(url) = resolve(&href, &base) else {
            continue;
        };
        if crate::companies::normalize_domain(&url).as_deref() != Some(domain.as_str()) {
            continue;
        }
        if !interesting(&url) || !visited.insert(url.clone()) {
            continue;
        }
        if !robots::allowed(&path_of(&url), &disallow) {
            continue;
        }
        if let Ok(page) = fetcher.fetch(&url).await
            && page.status == 200
        {
            pages.push(CrawledPage {
                url,
                text: html::strip_tags(&page.body),
            });
            extra_count += 1;
        }
    }
    Ok(pages)
}

fn resolve(href: &str, base: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty()
        || href.starts_with('#')
        || href.starts_with("mailto:")
        || href.starts_with("javascript:")
    {
        return None;
    }
    if href.starts_with("http://") || href.starts_with("https://") {
        return Some(href.to_owned());
    }
    if let Some(rest) = href.strip_prefix("//") {
        return Some(format!("https://{rest}"));
    }
    if href.starts_with('/') {
        return Some(format!("{base}{href}"));
    }
    Some(format!("{base}/{href}"))
}

fn path_of(url: &str) -> String {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    after_scheme
        .split_once('/')
        .map_or_else(|| "/".to_owned(), |(_, path)| format!("/{path}"))
}

fn interesting(url: &str) -> bool {
    let lower = url.to_lowercase();
    ["about", "product", "contact"]
        .iter()
        .any(|k| lower.contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_and_absolute_links() {
        let base = "https://example.com";
        assert_eq!(
            resolve("/about", base).as_deref(),
            Some("https://example.com/about")
        );
        assert_eq!(
            resolve("contact", base).as_deref(),
            Some("https://example.com/contact")
        );
        assert_eq!(
            resolve("https://other.example/x", base).as_deref(),
            Some("https://other.example/x")
        );
        assert_eq!(resolve("mailto:a@example.com", base), None);
        assert_eq!(resolve("#top", base), None);
    }

    #[test]
    fn extracts_the_path_for_robots_matching() {
        assert_eq!(path_of("https://example.com/about/team"), "/about/team");
        assert_eq!(path_of("https://example.com"), "/");
        assert_eq!(path_of("https://example.com/"), "/");
    }

    #[test]
    fn flags_interesting_pages_only() {
        assert!(interesting("https://example.com/About-Us"));
        assert!(interesting("https://example.com/products/widgets"));
        assert!(interesting("https://example.com/Contact"));
        assert!(!interesting("https://example.com/blog/2026"));
    }
}
