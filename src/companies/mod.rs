//! Discovered companies: the dedupe/merge identity (normalized domain, else
//! `(jurisdiction, state_file_number)`), and `company_sources` provenance.
//!
//! A company is never duplicated across sources or re-imports. It is looked
//! up first by domain (when the new record states one), then by state file
//! number; whichever is found gets any `NULL` column filled in from the new
//! record, and nothing is overwritten. So a company first seen from a filing
//! (file number, no domain) and later enriched with a domain, or seen again
//! from a different source, stays one row.

mod filter;

use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqliteConnection};
use utoipa::ToSchema;

use crate::error::AppResult;

pub use filter::{FilterOutcome, FilterStatus, apply_cheap_filter};

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Officer {
    pub name: String,
    pub title: Option<String>,
    pub address: Option<String>,
}

/// A company as reported by one source record. `website`, when present, is
/// turned into a normalized `domain` before the dedupe lookup.
#[derive(Debug, Clone, Default)]
pub struct NewCompany {
    pub name: String,
    pub entity_type: Option<String>,
    pub jurisdiction: String,
    pub state_file_number: Option<String>,
    /// Any timestamp `time::normalize` accepts; unparseable input is dropped.
    pub formation_date: Option<String>,
    pub status: Option<String>,
    pub principal_address: Option<String>,
    pub mailing_address: Option<String>,
    pub officers: Option<Vec<Officer>>,
    pub registered_agent: Option<String>,
    pub website: Option<String>,
    pub phone: Option<String>,
    pub category: Option<String>,
}

#[derive(Debug, Clone, Serialize, FromRow, ToSchema)]
pub struct Company {
    pub id: i64,
    pub name: String,
    pub normalized_name: String,
    pub entity_type: Option<String>,
    pub jurisdiction: String,
    pub state_file_number: Option<String>,
    pub formation_date: Option<String>,
    pub status: Option<String>,
    pub principal_address: Option<String>,
    pub mailing_address: Option<String>,
    /// `null` if none is known yet.
    #[schema(value_type = Option<Vec<Officer>>)]
    pub officers: Option<sqlx::types::Json<Vec<Officer>>>,
    pub registered_agent: Option<String>,
    pub website: Option<String>,
    /// Normalized: lowercase, no scheme, no leading `www.`, no path or port.
    pub domain: Option<String>,
    pub phone: Option<String>,
    pub category: Option<String>,
    pub filter_status: FilterStatus,
    pub filter_reason: Option<String>,
    pub pre_score: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct CompanySource {
    pub id: i64,
    pub company_id: i64,
    pub source: String,
    pub source_record_key: String,
    pub raw_file_path: String,
    pub raw_row: String,
    pub imported_at: String,
}

/// Lowercase, collapse whitespace, drop anything that is not a letter or
/// digit. Used for display-independent name lookups; identity dedupe uses
/// `domain` or `state_file_number`, never this.
pub fn normalize_name(name: &str) -> String {
    let lower: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    lower.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `https://Www.Example.com:8080/path?q=1` -> `example.com`. `None` for
/// anything that does not contain a host.
pub fn normalize_domain(input: &str) -> Option<String> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    let without_scheme = s.split_once("://").map_or(s, |(_, rest)| rest);
    let host = without_scheme.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.split('@').next_back().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    let host = host.to_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let host = host.trim_end_matches('.');
    (!host.is_empty() && host.contains('.')).then(|| host.to_owned())
}

async fn find_by_domain(conn: &mut SqliteConnection, domain: &str) -> AppResult<Option<Company>> {
    Ok(
        sqlx::query_as::<_, Company>("SELECT * FROM companies WHERE domain = ?1")
            .bind(domain)
            .fetch_optional(conn)
            .await?,
    )
}

async fn find_by_file_number(
    conn: &mut SqliteConnection,
    jurisdiction: &str,
    file_number: &str,
) -> AppResult<Option<Company>> {
    Ok(sqlx::query_as::<_, Company>(
        "SELECT * FROM companies WHERE jurisdiction = ?1 AND state_file_number = ?2",
    )
    .bind(jurisdiction)
    .bind(file_number)
    .fetch_optional(conn)
    .await?)
}

async fn insert(
    conn: &mut SqliteConnection,
    new: &NewCompany,
    domain: Option<&str>,
) -> AppResult<Company> {
    let formation_date = new
        .formation_date
        .as_deref()
        .and_then(crate::time::normalize);
    Ok(sqlx::query_as::<_, Company>(
        "INSERT INTO companies
           (name, normalized_name, entity_type, jurisdiction, state_file_number,
            formation_date, status, principal_address, mailing_address, officers,
            registered_agent, website, domain, phone, category)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         RETURNING *",
    )
    .bind(&new.name)
    .bind(normalize_name(&new.name))
    .bind(&new.entity_type)
    .bind(&new.jurisdiction)
    .bind(&new.state_file_number)
    .bind(formation_date)
    .bind(&new.status)
    .bind(&new.principal_address)
    .bind(&new.mailing_address)
    .bind(new.officers.clone().map(sqlx::types::Json))
    .bind(&new.registered_agent)
    .bind(&new.website)
    .bind(domain)
    .bind(&new.phone)
    .bind(&new.category)
    .fetch_one(conn)
    .await?)
}

/// Fill any `NULL` column on `existing` from `new`; never overwrite a value
/// already on file.
async fn fill_missing(
    conn: &mut SqliteConnection,
    existing: &Company,
    new: &NewCompany,
    domain: Option<&str>,
) -> AppResult<Company> {
    let formation_date = new
        .formation_date
        .as_deref()
        .and_then(crate::time::normalize);
    Ok(sqlx::query_as::<_, Company>(
        "UPDATE companies SET
           entity_type = COALESCE(entity_type, ?1),
           state_file_number = COALESCE(state_file_number, ?2),
           formation_date = COALESCE(formation_date, ?3),
           status = COALESCE(status, ?4),
           principal_address = COALESCE(principal_address, ?5),
           mailing_address = COALESCE(mailing_address, ?6),
           officers = COALESCE(officers, ?7),
           registered_agent = COALESCE(registered_agent, ?8),
           website = COALESCE(website, ?9),
           domain = COALESCE(domain, ?10),
           phone = COALESCE(phone, ?11),
           category = COALESCE(category, ?12),
           updated_at = CURRENT_TIMESTAMP
         WHERE id = ?13
         RETURNING *",
    )
    .bind(&new.entity_type)
    .bind(&new.state_file_number)
    .bind(formation_date)
    .bind(&new.status)
    .bind(&new.principal_address)
    .bind(&new.mailing_address)
    .bind(new.officers.clone().map(sqlx::types::Json))
    .bind(&new.registered_agent)
    .bind(&new.website)
    .bind(domain)
    .bind(&new.phone)
    .bind(&new.category)
    .bind(existing.id)
    .fetch_one(conn)
    .await?)
}

/// Find-or-create a company by domain (preferred) or `(jurisdiction,
/// state_file_number)`, filling in whatever was previously unknown. A new
/// record with neither identity is always inserted as a new row: there is
/// nothing to dedupe it against.
pub async fn upsert(conn: &mut SqliteConnection, new: NewCompany) -> AppResult<Company> {
    let domain = new.website.as_deref().and_then(normalize_domain);

    if let Some(domain) = domain.as_deref()
        && let Some(existing) = find_by_domain(conn, domain).await?
    {
        return fill_missing(conn, &existing, &new, Some(domain)).await;
    }
    if let Some(file_number) = new.state_file_number.as_deref()
        && let Some(existing) = find_by_file_number(conn, &new.jurisdiction, file_number).await?
    {
        return fill_missing(conn, &existing, &new, domain.as_deref()).await;
    }
    insert(conn, &new, domain.as_deref()).await
}

pub async fn fetch(conn: &mut SqliteConnection, id: i64) -> AppResult<Company> {
    sqlx::query_as::<_, Company>("SELECT * FROM companies WHERE id = ?1")
        .bind(id)
        .fetch_optional(conn)
        .await?
        .ok_or(crate::error::AppError::NotFound("company"))
}

pub async fn set_filter_outcome(
    conn: &mut SqliteConnection,
    id: i64,
    outcome: &FilterOutcome,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE companies SET filter_status = ?1, filter_reason = ?2, pre_score = ?3,
           updated_at = CURRENT_TIMESTAMP
         WHERE id = ?4",
    )
    .bind(outcome.status)
    .bind(&outcome.reason)
    .bind(outcome.pre_score)
    .bind(id)
    .execute(conn)
    .await?;
    Ok(())
}

/// Fill in `website`/`phone`/`category` from a places lookup, and `domain`
/// from `website` if it normalizes. Never overwrites an existing value.
pub async fn fill_contact_info(
    conn: &mut SqliteConnection,
    id: i64,
    website: Option<&str>,
    phone: Option<&str>,
    category: Option<&str>,
) -> AppResult<Company> {
    let domain = website.and_then(normalize_domain);
    Ok(sqlx::query_as::<_, Company>(
        "UPDATE companies SET
           website = COALESCE(website, ?1),
           phone = COALESCE(phone, ?2),
           category = COALESCE(category, ?3),
           domain = COALESCE(domain, ?4),
           updated_at = CURRENT_TIMESTAMP
         WHERE id = ?5
         RETURNING *",
    )
    .bind(website)
    .bind(phone)
    .bind(category)
    .bind(domain)
    .bind(id)
    .fetch_one(conn)
    .await?)
}

/// Companies that passed the cheap filter and have no `company_enrichments`
/// row of `kind` yet, oldest first.
pub async fn list_without_enrichment(
    conn: &mut SqliteConnection,
    kind: &str,
    limit: i64,
) -> AppResult<Vec<Company>> {
    Ok(sqlx::query_as::<_, Company>(
        "SELECT * FROM companies
         WHERE filter_status = 'passed'
           AND NOT EXISTS (
             SELECT 1 FROM company_enrichments
             WHERE company_enrichments.company_id = companies.id
               AND company_enrichments.kind = ?1
           )
         ORDER BY id
         LIMIT ?2",
    )
    .bind(kind)
    .bind(limit)
    .fetch_all(conn)
    .await?)
}

pub struct NewCompanySource {
    pub company_id: i64,
    pub source: String,
    pub source_record_key: String,
    pub raw_file_path: String,
    pub raw_row: serde_json::Value,
}

/// Record a source row for a company. Idempotent on `(source,
/// source_record_key)`: a second call with the same key is a no-op and
/// returns `None`.
pub async fn record_source(
    conn: &mut SqliteConnection,
    new: NewCompanySource,
) -> AppResult<Option<CompanySource>> {
    Ok(sqlx::query_as::<_, CompanySource>(
        "INSERT INTO company_sources (company_id, source, source_record_key, raw_file_path, raw_row)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (source, source_record_key) DO NOTHING
         RETURNING *",
    )
    .bind(new.company_id)
    .bind(new.source)
    .bind(new.source_record_key)
    .bind(new.raw_file_path)
    .bind(new.raw_row.to_string())
    .fetch_optional(conn)
    .await?)
}

/// Whether `(source, source_record_key)` has already been imported.
pub async fn source_exists(
    conn: &mut SqliteConnection,
    source: &str,
    source_record_key: &str,
) -> AppResult<bool> {
    let id: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM company_sources WHERE source = ?1 AND source_record_key = ?2",
    )
    .bind(source)
    .bind(source_record_key)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(id.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_names() {
        assert_eq!(normalize_name("  Acme, Inc.  "), "acme inc");
        assert_eq!(normalize_name("Böx Co."), "böx co");
    }

    #[test]
    fn normalizes_domains() {
        let cases = [
            ("https://Www.Example.com/path?q=1", Some("example.com")),
            ("http://example.com:8080", Some("example.com")),
            ("example.com", Some("example.com")),
            ("WWW.EXAMPLE.COM.", Some("example.com")),
            ("", None),
            ("not-a-domain", None),
            (
                "https://user:pass@sub.example.com/",
                Some("sub.example.com"),
            ),
        ];
        for (input, want) in cases {
            assert_eq!(normalize_domain(input).as_deref(), want, "input {input:?}");
        }
    }
}
