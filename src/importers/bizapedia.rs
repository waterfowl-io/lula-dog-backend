//! Bizapedia Pro Data CSV importer. Bizapedia's exact column names are not
//! yet known (access starts the day after this was written; see
//! docs/lead-gen.md), so the mapping from CSV header to field is
//! configurable: a TOML file of `field = ["alias one", "alias two"]` can
//! override any subset of fields, matched case/whitespace-insensitively.
//! Unmapped fields fall back to a best guess built from Bizapedia's
//! documented record (filing date, entity name, entity type, file number,
//! status, principal/mailing address, registered agent, officers/managers).

use std::path::Path;

use chrono::Utc;
use serde::Deserialize;
use sqlx::SqlitePool;

use super::{ImportSummary, normalize_header};
use crate::bucket::Bucket;
use crate::companies::{self, NewCompany, Officer};
use crate::coverage::{self, NewCoverage};
use crate::target_profiles::TargetProfile;

/// Best-guess default aliases, built from Bizapedia's documented fields.
/// Each list is matched case/whitespace-insensitively against the CSV
/// header row; a `--mapping` file can override any subset.
mod defaults {
    pub fn entity_name() -> Vec<String> {
        ["entity name", "company name", "name", "business name"]
            .map(str::to_owned)
            .to_vec()
    }
    pub fn entity_type() -> Vec<String> {
        ["entity type", "type", "entity"]
            .map(str::to_owned)
            .to_vec()
    }
    pub fn file_number() -> Vec<String> {
        [
            "file number",
            "file #",
            "filing number",
            "sos file number",
            "state file number",
        ]
        .map(str::to_owned)
        .to_vec()
    }
    pub fn filing_date() -> Vec<String> {
        [
            "filing date",
            "date filed",
            "incorporation date",
            "formation date",
        ]
        .map(str::to_owned)
        .to_vec()
    }
    pub fn status() -> Vec<String> {
        ["status", "entity status"].map(str::to_owned).to_vec()
    }
    pub fn principal_address() -> Vec<String> {
        ["principal address", "principal office address", "address"]
            .map(str::to_owned)
            .to_vec()
    }
    pub fn mailing_address() -> Vec<String> {
        ["mailing address"].map(str::to_owned).to_vec()
    }
    pub fn registered_agent() -> Vec<String> {
        [
            "registered agent",
            "registered agent name",
            "agent name",
            "agent",
        ]
        .map(str::to_owned)
        .to_vec()
    }
    pub fn officers() -> Vec<String> {
        [
            "officers",
            "officers/managers",
            "managers",
            "officer/manager",
        ]
        .map(str::to_owned)
        .to_vec()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ColumnMapping {
    #[serde(default = "defaults::entity_name")]
    pub entity_name: Vec<String>,
    #[serde(default = "defaults::entity_type")]
    pub entity_type: Vec<String>,
    #[serde(default = "defaults::file_number")]
    pub file_number: Vec<String>,
    #[serde(default = "defaults::filing_date")]
    pub filing_date: Vec<String>,
    #[serde(default = "defaults::status")]
    pub status: Vec<String>,
    #[serde(default = "defaults::principal_address")]
    pub principal_address: Vec<String>,
    #[serde(default = "defaults::mailing_address")]
    pub mailing_address: Vec<String>,
    #[serde(default = "defaults::registered_agent")]
    pub registered_agent: Vec<String>,
    #[serde(default = "defaults::officers")]
    pub officers: Vec<String>,
}

impl Default for ColumnMapping {
    fn default() -> Self {
        Self {
            entity_name: defaults::entity_name(),
            entity_type: defaults::entity_type(),
            file_number: defaults::file_number(),
            filing_date: defaults::filing_date(),
            status: defaults::status(),
            principal_address: defaults::principal_address(),
            mailing_address: defaults::mailing_address(),
            registered_agent: defaults::registered_agent(),
            officers: defaults::officers(),
        }
    }
}

impl ColumnMapping {
    pub fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        let Some(path) = path else {
            return Ok(Self::default());
        };
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("reading mapping file {}: {e}", path.display()))?;
        toml::from_str(&text)
            .map_err(|e| anyhow::anyhow!("parsing mapping file {}: {e}", path.display()))
    }

    fn fields(&self) -> [(&'static str, &[String]); 9] {
        [
            ("entity_name", &self.entity_name),
            ("entity_type", &self.entity_type),
            ("file_number", &self.file_number),
            ("filing_date", &self.filing_date),
            ("status", &self.status),
            ("principal_address", &self.principal_address),
            ("mailing_address", &self.mailing_address),
            ("registered_agent", &self.registered_agent),
            ("officers", &self.officers),
        ]
    }
}

/// `headers[field]` is the CSV column index for that field, when the header
/// row has a column matching one of its aliases.
struct ColumnIndex {
    by_field: std::collections::HashMap<&'static str, usize>,
}

impl ColumnIndex {
    fn resolve(headers: &csv::StringRecord, mapping: &ColumnMapping) -> Self {
        let normalized: Vec<String> = headers.iter().map(normalize_header).collect();
        let mut by_field = std::collections::HashMap::new();
        for (field, aliases) in mapping.fields() {
            let aliases: Vec<String> = aliases.iter().map(|a| normalize_header(a)).collect();
            if let Some(idx) = normalized.iter().position(|h| aliases.contains(h)) {
                by_field.insert(field, idx);
            }
        }
        Self { by_field }
    }

    fn get<'r>(&self, record: &'r csv::StringRecord, field: &str) -> Option<&'r str> {
        self.by_field
            .get(field)
            .and_then(|&idx| record.get(idx))
            .map(str::trim)
            .filter(|v| !v.is_empty())
    }
}

/// Officers are an unstructured list on Bizapedia; split on common
/// separators and keep the name only, since the field's shape is unknown
/// until Nolan has access. Revisit once real data is in hand.
fn parse_officers(raw: &str) -> Vec<Officer> {
    raw.split([';', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|name| Officer {
            name: name.to_owned(),
            title: None,
            address: None,
        })
        .collect()
}

fn source_record_key(
    file_number: Option<&str>,
    entity_name: &str,
    filing_date: Option<&str>,
) -> String {
    match file_number {
        Some(n) if !n.is_empty() => n.to_owned(),
        _ => format!(
            "{}|{}",
            companies::normalize_name(entity_name),
            filing_date.unwrap_or("")
        ),
    }
}

fn row_to_json(headers: &csv::StringRecord, record: &csv::StringRecord) -> serde_json::Value {
    let map: serde_json::Map<String, serde_json::Value> = headers
        .iter()
        .zip(record.iter())
        .map(|(h, v)| (h.to_owned(), serde_json::Value::String(v.to_owned())))
        .collect();
    serde_json::Value::Object(map)
}

/// Keyword lists scored against a company name by the cheap filter, taken
/// from the active target profile (empty when there is none yet).
#[derive(Debug, Clone, Default)]
struct ProfileKeywords {
    include: Vec<String>,
    industries: Vec<String>,
    exclude: Vec<String>,
}

impl From<Option<&TargetProfile>> for ProfileKeywords {
    fn from(profile: Option<&TargetProfile>) -> Self {
        profile.map_or_else(Self::default, |p| Self {
            include: p.include_keywords.0.clone(),
            industries: p.industries.0.clone(),
            exclude: p.exclude_keywords.0.clone(),
        })
    }
}

enum RowOutcome {
    New { filtered: bool },
    Duplicate,
    NoName,
}

async fn process_row(
    tx: &mut sqlx::SqliteConnection,
    headers: &csv::StringRecord,
    record: &csv::StringRecord,
    index: &ColumnIndex,
    raw_file_path: &str,
    keywords: &ProfileKeywords,
) -> anyhow::Result<RowOutcome> {
    let Some(entity_name) = index.get(record, "entity_name") else {
        return Ok(RowOutcome::NoName);
    };
    let file_number = index.get(record, "file_number");
    let filing_date = index.get(record, "filing_date");
    let key = source_record_key(file_number, entity_name, filing_date);

    if companies::source_exists(tx, "bizapedia", &key).await? {
        return Ok(RowOutcome::Duplicate);
    }

    let new_company = NewCompany {
        name: entity_name.to_owned(),
        entity_type: index.get(record, "entity_type").map(str::to_owned),
        jurisdiction: "IL".to_owned(),
        state_file_number: file_number.map(str::to_owned),
        formation_date: filing_date.map(str::to_owned),
        status: index.get(record, "status").map(str::to_owned),
        principal_address: index.get(record, "principal_address").map(str::to_owned),
        mailing_address: index.get(record, "mailing_address").map(str::to_owned),
        officers: index.get(record, "officers").map(parse_officers),
        registered_agent: index.get(record, "registered_agent").map(str::to_owned),
        website: None,
        phone: None,
        category: None,
    };
    let company = companies::upsert(tx, new_company).await?;

    companies::record_source(
        tx,
        companies::NewCompanySource {
            company_id: company.id,
            source: "bizapedia".to_owned(),
            source_record_key: key,
            raw_file_path: raw_file_path.to_owned(),
            raw_row: row_to_json(headers, record),
        },
    )
    .await?;

    let mut filtered = false;
    if company.filter_status == companies::FilterStatus::Pending {
        let outcome = companies::apply_cheap_filter(
            &company.name,
            company.status.as_deref(),
            company.registered_agent.as_deref(),
            &keywords.include,
            &keywords.industries,
            &keywords.exclude,
        );
        filtered = outcome.status == companies::FilterStatus::Filtered;
        companies::set_filter_outcome(tx, company.id, &outcome).await?;
    }
    Ok(RowOutcome::New { filtered })
}

/// Import a Bizapedia CSV export. Copies the file into the bucket unchanged,
/// then processes each row: idempotent on `(bizapedia, source_record_key)`,
/// so re-importing the same file (or one with overlapping rows) never
/// duplicates a company, a source record or a lead candidate. Runs the
/// cheap filter once per newly-seen company against `profile`.
pub async fn import(
    pool: &SqlitePool,
    bucket: &Bucket,
    csv_path: &Path,
    mapping: &ColumnMapping,
    profile: Option<&TargetProfile>,
) -> anyhow::Result<ImportSummary> {
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let raw_file_path = bucket.store_source_file("bizapedia", &today, csv_path)?;
    let file_name = csv_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut reader = csv::Reader::from_path(csv_path)?;
    let headers = reader.headers()?.clone();
    let index = ColumnIndex::resolve(&headers, mapping);
    let keywords = ProfileKeywords::from(profile);

    let mut summary = ImportSummary::default();
    let mut tx = pool.begin().await?;
    for result in reader.records() {
        summary.seen += 1;
        let Ok(record) = result else {
            summary.error += 1;
            continue;
        };
        match process_row(
            &mut tx,
            &headers,
            &record,
            &index,
            &raw_file_path,
            &keywords,
        )
        .await?
        {
            RowOutcome::New { filtered } => {
                summary.new += 1;
                if filtered {
                    summary.filtered += 1;
                }
            }
            RowOutcome::Duplicate => summary.duplicate += 1,
            RowOutcome::NoName => summary.error += 1,
        }
    }

    coverage::record(
        &mut tx,
        NewCoverage {
            source: "bizapedia",
            region: Some("IL"),
            period: Some(&file_name),
            ..Default::default()
        },
        summary.into(),
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(summary)
}
