//! `enrichment_runs` and `company_enrichments` bookkeeping, and the
//! `enrich`/`classify` CLI pipelines that use it: Places lookup, crawl and
//! classify each check for an existing `company_enrichments` row before
//! doing the work, so a re-run only processes what is new.

use chrono::Utc;
use serde_json::Value;
use sqlx::{FromRow, SqliteConnection, SqlitePool};

use crate::bucket::Bucket;
use crate::classifier::{ClassificationInput, Classifier};
use crate::companies::{self, Company};
use crate::crawler::{self, CrawlLimits, Fetcher};
use crate::error::AppResult;
use crate::leads;
use crate::places::{PlaceQuery, PlacesClient};
use crate::target_profiles::TargetProfile;

#[derive(Debug, Clone, Copy, Default)]
pub struct Counts {
    pub attempted: i64,
    pub success: i64,
    pub error: i64,
}

#[derive(Debug, FromRow)]
struct EnrichmentRun {
    id: i64,
}

async fn start_run(conn: &mut SqliteConnection, kind: &str) -> AppResult<i64> {
    let run: EnrichmentRun =
        sqlx::query_as("INSERT INTO enrichment_runs (kind) VALUES (?1) RETURNING id")
            .bind(kind)
            .fetch_one(conn)
            .await?;
    Ok(run.id)
}

async fn finish_run(conn: &mut SqliteConnection, run_id: i64, counts: Counts) -> AppResult<()> {
    sqlx::query(
        "UPDATE enrichment_runs SET finished_at = CURRENT_TIMESTAMP,
           attempted_count = ?1, success_count = ?2, error_count = ?3
         WHERE id = ?4",
    )
    .bind(counts.attempted)
    .bind(counts.success)
    .bind(counts.error)
    .bind(run_id)
    .execute(conn)
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn record(
    conn: &mut SqliteConnection,
    company_id: i64,
    kind: &str,
    run_id: i64,
    result: Option<&Value>,
    files: &[String],
    status: &str,
    error: Option<&str>,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO company_enrichments
           (company_id, kind, run_id, result, files, status, error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )
    .bind(company_id)
    .bind(kind)
    .bind(run_id)
    .bind(result.map(ToString::to_string))
    .bind(serde_json::to_string(files).unwrap_or_else(|_| "[]".to_owned()))
    .bind(status)
    .bind(error)
    .execute(conn)
    .await?;
    Ok(())
}

/// The most recent `kind` result for a company, as raw JSON text.
async fn latest_result(
    conn: &mut SqliteConnection,
    company_id: i64,
    kind: &str,
) -> AppResult<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT result FROM company_enrichments
         WHERE company_id = ?1 AND kind = ?2
         ORDER BY id DESC LIMIT 1",
    )
    .bind(company_id)
    .bind(kind)
    .fetch_optional(conn)
    .await?
    .flatten())
}

async fn places_phase(
    conn: &mut SqliteConnection,
    places: &dyn PlacesClient,
    run_id: i64,
    limit: i64,
) -> anyhow::Result<Counts> {
    let mut counts = Counts::default();
    for company in companies::list_without_enrichment(conn, "places", limit).await? {
        counts.attempted += 1;
        let query = PlaceQuery {
            name: &company.name,
            address: company.principal_address.as_deref(),
        };
        match places.search(query).await {
            Ok(found) => {
                if let Some(result) = &found {
                    companies::fill_contact_info(
                        conn,
                        company.id,
                        result.website.as_deref(),
                        result.phone.as_deref(),
                        result.category.as_deref(),
                    )
                    .await?;
                }
                let result = found.map(|r| {
                    serde_json::json!({"website": r.website, "phone": r.phone, "category": r.category})
                });
                record(
                    conn,
                    company.id,
                    "places",
                    run_id,
                    result.as_ref(),
                    &[],
                    "ok",
                    None,
                )
                .await?;
                counts.success += 1;
            }
            Err(e) => {
                record(
                    conn,
                    company.id,
                    "places",
                    run_id,
                    None,
                    &[],
                    "error",
                    Some(&e.to_string()),
                )
                .await?;
                counts.error += 1;
            }
        }
    }
    Ok(counts)
}

async fn crawl_one(
    conn: &mut SqliteConnection,
    bucket: &Bucket,
    fetcher: &dyn Fetcher,
    limits: CrawlLimits,
    run_id: i64,
    company: &Company,
) -> anyhow::Result<bool> {
    let Some(website) = company.website.as_deref() else {
        return Ok(false);
    };
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let domain = company.domain.clone().unwrap_or_else(|| website.to_owned());
    match crawler::crawl(fetcher, website, limits).await {
        Ok(pages) => {
            let mut files = Vec::new();
            let mut summaries = Vec::new();
            for (i, page) in pages.iter().enumerate() {
                let slug = format!("page-{i}");
                files.push(bucket.write_crawl_text(&domain, &today, &slug, &page.text)?);
                summaries.push(serde_json::json!({"url": page.url, "text": page.text}));
            }
            let result = serde_json::json!({"page_count": pages.len(), "pages": summaries});
            record(
                conn,
                company.id,
                "crawl",
                run_id,
                Some(&result),
                &files,
                "ok",
                None,
            )
            .await?;
            Ok(true)
        }
        Err(e) => {
            record(
                conn,
                company.id,
                "crawl",
                run_id,
                None,
                &[],
                "error",
                Some(&e.to_string()),
            )
            .await?;
            Ok(false)
        }
    }
}

async fn crawl_phase(
    conn: &mut SqliteConnection,
    bucket: &Bucket,
    fetcher: &dyn Fetcher,
    limits: CrawlLimits,
    run_id: i64,
    limit: i64,
) -> anyhow::Result<Counts> {
    let mut counts = Counts::default();
    let candidates: Vec<Company> = companies::list_without_enrichment(conn, "crawl", limit)
        .await?
        .into_iter()
        .filter(|c| c.website.is_some())
        .collect();
    for company in candidates {
        counts.attempted += 1;
        if crawl_one(conn, bucket, fetcher, limits, run_id, &company).await? {
            counts.success += 1;
        } else {
            counts.error += 1;
        }
    }
    Ok(counts)
}

#[derive(Debug, Clone, Copy, Default)]
pub struct EnrichSummary {
    pub places: Counts,
    pub crawl: Counts,
}

/// Places lookup, then crawl, for companies that passed the cheap filter
/// and have not yet been through each step. `limit` applies per step.
pub async fn run_enrich(
    pool: &SqlitePool,
    bucket: &Bucket,
    places: &dyn PlacesClient,
    fetcher: &dyn Fetcher,
    limits: CrawlLimits,
    limit: i64,
) -> anyhow::Result<EnrichSummary> {
    let mut conn = pool.acquire().await?;

    let places_run = start_run(&mut conn, "places").await?;
    let places_counts = places_phase(&mut conn, places, places_run, limit).await?;
    finish_run(&mut conn, places_run, places_counts).await?;

    let crawl_run = start_run(&mut conn, "crawl").await?;
    let crawl_counts = crawl_phase(&mut conn, bucket, fetcher, limits, crawl_run, limit).await?;
    finish_run(&mut conn, crawl_run, crawl_counts).await?;

    Ok(EnrichSummary {
        places: places_counts,
        crawl: crawl_counts,
    })
}

/// Text from the most recent crawl, and a contact-page URL hint if one of
/// the crawled pages looks like one.
async fn crawl_context(
    conn: &mut SqliteConnection,
    company_id: i64,
) -> AppResult<(String, Option<String>)> {
    let Some(raw) = latest_result(conn, company_id, "crawl").await? else {
        return Ok((String::new(), None));
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&raw) else {
        return Ok((String::new(), None));
    };
    let pages = parsed
        .get("pages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let text = pages
        .iter()
        .filter_map(|p| p.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n\n");
    let contact_hint = pages
        .iter()
        .filter_map(|p| p.get("url").and_then(Value::as_str))
        .find(|u| u.to_lowercase().contains("contact"))
        .map(str::to_owned);
    Ok((text, contact_hint))
}

/// Classify companies that passed the cheap filter and have not yet been
/// classified, queueing those at or above `profile.min_fit_score` as lead
/// candidates (never duplicating an already-open one).
pub async fn run_classify(
    pool: &SqlitePool,
    classifier: &dyn Classifier,
    profile: &TargetProfile,
    limit: i64,
) -> anyhow::Result<Counts> {
    let mut conn = pool.acquire().await?;
    let run_id = start_run(&mut conn, "classify").await?;
    let mut counts = Counts::default();

    for company in companies::list_without_enrichment(&mut conn, "classify", limit).await? {
        counts.attempted += 1;
        let (crawled_text, contact_hint) = crawl_context(&mut conn, company.id).await?;
        let input = ClassificationInput {
            company: &company,
            profile,
            crawled_text: &crawled_text,
            contact_hint: contact_hint.as_deref(),
        };
        match classifier.classify(input).await {
            Ok(classification) => {
                let classification = classification.sanitize();
                let result = serde_json::to_value(&classification)?;
                record(
                    &mut conn,
                    company.id,
                    "classify",
                    run_id,
                    Some(&result),
                    &[],
                    "ok",
                    None,
                )
                .await?;
                if classification.fit_score >= profile.min_fit_score {
                    leads::queue_candidate(
                        &mut conn,
                        company.id,
                        profile.id,
                        classification.fit_score,
                        classification.fit_reason.as_deref(),
                    )
                    .await?;
                }
                counts.success += 1;
            }
            Err(e) => {
                record(
                    &mut conn,
                    company.id,
                    "classify",
                    run_id,
                    None,
                    &[],
                    "error",
                    Some(&e.to_string()),
                )
                .await?;
                counts.error += 1;
            }
        }
    }
    finish_run(&mut conn, run_id, counts).await?;
    Ok(counts)
}
