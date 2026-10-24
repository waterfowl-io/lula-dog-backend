//! The crawler against a loopback fake server (no real network): robots.txt
//! handling, the same-domain/page-count cap, and the response size cap.

#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a failing assertion aborting the test is the mechanism"
)]

use std::time::Duration;

use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Router, body::Body};
use lula_dog_backend::crawler::{self, CrawlLimits, Fetcher, HttpFetcher};

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    format!("http://{addr}")
}

const HOME: &str = r#"<html><body>
    <h1>Acme Packaging</h1><p>We ship boxes nationwide.</p>
    <a href="/about">About</a> <a href="/contact">Contact</a> <a href="/blog">Blog</a>
    <a href="http://other.example/elsewhere">Somewhere else</a>
</body></html>"#;

fn default_limits() -> CrawlLimits {
    CrawlLimits {
        max_pages: 3,
        timeout_secs: 5,
        max_bytes: 1_000_000,
    }
}

#[tokio::test]
async fn crawls_the_home_page_and_same_domain_interesting_links() {
    let app = Router::new()
        .route("/robots.txt", get(|| async { StatusCode::NOT_FOUND }))
        .route("/", get(|| async { Body::from(HOME) }))
        .route(
            "/about",
            get(|| async { Body::from("<p>About us: packaging experts.</p>") }),
        )
        .route(
            "/contact",
            get(|| async { Body::from("<p>Contact: info@example.com</p>") }),
        )
        .route(
            "/blog",
            get(|| async { Body::from("<p>Not interesting</p>") }),
        );
    let base = serve(app).await;
    let fetcher = HttpFetcher::new(Duration::from_secs(5), 1_000_000).unwrap();

    let pages = crawler::crawl(&fetcher, &base, default_limits())
        .await
        .unwrap();

    let urls: Vec<&str> = pages.iter().map(|p| p.url.as_str()).collect();
    assert_eq!(urls.len(), 3, "{urls:?}");
    assert_eq!(urls[0], base);
    assert!(urls.contains(&format!("{base}/about").as_str()));
    assert!(urls.contains(&format!("{base}/contact").as_str()));
    assert!(
        !urls.iter().any(|u| u.contains("/blog")),
        "not an interesting page"
    );
    assert!(
        pages.iter().any(|p| p.text.contains("packaging experts")),
        "{pages:?}"
    );
}

#[tokio::test]
async fn robots_disallow_on_the_home_page_crawls_nothing() {
    let app = Router::new()
        .route(
            "/robots.txt",
            get(|| async { "User-agent: *\nDisallow: /\n" }),
        )
        .route("/", get(|| async { Body::from(HOME) }));
    let base = serve(app).await;
    let fetcher = HttpFetcher::new(Duration::from_secs(5), 1_000_000).unwrap();

    let pages = crawler::crawl(&fetcher, &base, default_limits())
        .await
        .unwrap();
    assert!(pages.is_empty(), "{pages:?}");
}

#[tokio::test]
async fn robots_disallow_on_a_specific_path_skips_only_that_page() {
    let app = Router::new()
        .route(
            "/robots.txt",
            get(|| async { "User-agent: *\nDisallow: /contact\n" }),
        )
        .route("/", get(|| async { Body::from(HOME) }))
        .route("/about", get(|| async { Body::from("<p>About us.</p>") }))
        .route(
            "/contact",
            get(|| async { Body::from("<p>Should be skipped.</p>") }),
        );
    let base = serve(app).await;
    let fetcher = HttpFetcher::new(Duration::from_secs(5), 1_000_000).unwrap();

    let pages = crawler::crawl(&fetcher, &base, default_limits())
        .await
        .unwrap();
    let urls: Vec<&str> = pages.iter().map(|p| p.url.as_str()).collect();
    assert!(urls.contains(&format!("{base}/about").as_str()), "{urls:?}");
    assert!(
        !urls.contains(&format!("{base}/contact").as_str()),
        "{urls:?}"
    );
}

#[tokio::test]
async fn max_pages_caps_the_total_including_the_home_page() {
    let app = Router::new()
        .route("/robots.txt", get(|| async { StatusCode::NOT_FOUND }))
        .route("/", get(|| async { Body::from(HOME) }))
        .route("/about", get(|| async { Body::from("<p>About.</p>") }))
        .route("/contact", get(|| async { Body::from("<p>Contact.</p>") }));
    let base = serve(app).await;
    let fetcher = HttpFetcher::new(Duration::from_secs(5), 1_000_000).unwrap();
    let limits = CrawlLimits {
        max_pages: 1,
        ..default_limits()
    };

    let pages = crawler::crawl(&fetcher, &base, limits).await.unwrap();
    assert_eq!(pages.len(), 1, "{pages:?}");
    assert_eq!(pages[0].url, base);
}

#[tokio::test]
async fn response_over_the_size_cap_via_content_length_is_rejected_unread() {
    let big = "a".repeat(10_000);
    let app = Router::new().route(
        "/big",
        get(move || {
            let big = big.clone();
            async move {
                let mut headers = HeaderMap::new();
                headers.insert("content-type", "text/plain".parse().unwrap());
                (headers, big).into_response()
            }
        }),
    );
    let base = serve(app).await;
    let fetcher = HttpFetcher::new(Duration::from_secs(5), 100).unwrap();

    // `axum` sets `Content-Length` for a sized body, so this hits the
    // reject-before-reading path: the body comes back empty, not truncated.
    let page = fetcher.fetch(&format!("{base}/big")).await.unwrap();
    assert_eq!(page.status, 200);
    assert_eq!(
        page.body.len(),
        0,
        "oversized body should be rejected unread"
    );
}
