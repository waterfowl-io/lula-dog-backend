//! Illinois Secretary of State LLC master file: a fixed-width mainframe
//! export, never downloaded by this importer (see docs/lead-gen.md for why
//! and for the next step on automated retrieval). The record layout below is
//! taken from the field positions published in
//! <https://github.com/fgregg/il-corporate-filings> (`schemas/llcallmst.schema`,
//! `llcallnam.schema`, `llcallagt.schema`) rather than guessed.
//!
//! `status_code` is stored as the raw two-letter mainframe code (e.g. `AC`):
//! the mapping from code to a human status ("active", "dissolved", ...) is
//! **unverified** and is not attempted here, so the cheap filter's
//! inactive-status check will not catch IL SOS-sourced companies yet. See
//! docs/lead-gen.md.
//!
//! Joins three files by `file_number`, expected alongside each other with
//! their conventional names from the upstream zip: `llcallmst.txt` (given,
//! the master record), `llcallnam.txt` (required, the entity name) and
//! `llcallagt.txt` (optional, the registered agent). Manager/officer records
//! (`llcallmgr.txt`) are not joined yet; see docs/lead-gen.md.

use std::collections::HashMap;
use std::path::Path;

use chrono::Utc;
use sqlx::SqlitePool;

use super::{ImportSummary, ProfileKeywords};
use crate::bucket::Bucket;
use crate::companies::{self, NewCompany};
use crate::coverage::{self, NewCoverage};
use crate::target_profiles::TargetProfile;

/// A fixed-width field at byte offset `start`, length `len`. `.get()` (not
/// indexing) so a short or malformed line yields `None` instead of a panic.
fn field(line: &str, start: usize, len: usize) -> Option<&str> {
    line.get(start..start + len)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// `YYYYMMDD`; `00000000` and blank both mean "not set".
fn mainframe_date(line: &str, start: usize) -> Option<String> {
    let raw = field(line, start, 8)?;
    if raw == "00000000" {
        return None;
    }
    let y = raw.get(0..4)?;
    let m = raw.get(4..6)?;
    let d = raw.get(6..8)?;
    crate::time::normalize(&format!("{y}-{m}-{d}"))
}

struct MasterRow {
    file_number: String,
    status_code: Option<String>,
    organized_date: Option<String>,
    records_off_street: Option<String>,
    records_off_city: Option<String>,
    records_off_zip: Option<String>,
}

fn parse_master_line(line: &str) -> Option<MasterRow> {
    Some(MasterRow {
        file_number: field(line, 0, 8)?.to_owned(),
        status_code: field(line, 14, 2).map(str::to_owned),
        organized_date: mainframe_date(line, 24),
        records_off_street: field(line, 43, 45).map(str::to_owned),
        records_off_city: field(line, 88, 30).map(str::to_owned),
        records_off_zip: field(line, 118, 9).map(str::to_owned),
    })
}

fn parse_name_line(line: &str) -> Option<(String, String)> {
    Some((
        field(line, 0, 8)?.to_owned(),
        field(line, 8, 120)?.to_owned(),
    ))
}

fn parse_agent_line(line: &str) -> Option<(String, String)> {
    Some((
        field(line, 0, 8)?.to_owned(),
        field(line, 9, 60)?.to_owned(),
    ))
}

fn load_index(
    path: &Path,
    parse: impl Fn(&str) -> Option<(String, String)>,
) -> anyhow::Result<HashMap<String, String>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
    let mut index = HashMap::new();
    for line in text.lines() {
        if let Some((file_number, value)) = parse(line) {
            index.entry(file_number).or_insert(value);
        }
    }
    Ok(index)
}

fn records_office(row: &MasterRow) -> Option<String> {
    let parts: Vec<&str> = [
        row.records_off_street.as_deref(),
        row.records_off_city.as_deref(),
        row.records_off_zip.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

enum RowOutcome {
    New { filtered: bool },
    Duplicate,
    NoNameOrUnparseable,
}

async fn process_row(
    tx: &mut sqlx::SqliteConnection,
    line: &str,
    names: &HashMap<String, String>,
    agents: &HashMap<String, String>,
    raw_file_path: &str,
    keywords: &ProfileKeywords,
) -> anyhow::Result<RowOutcome> {
    let Some(row) = parse_master_line(line) else {
        return Ok(RowOutcome::NoNameOrUnparseable);
    };
    let Some(name) = names.get(&row.file_number) else {
        return Ok(RowOutcome::NoNameOrUnparseable);
    };

    if companies::source_exists(tx, "il_sos_llc", &row.file_number).await? {
        return Ok(RowOutcome::Duplicate);
    }

    let new_company = NewCompany {
        name: name.clone(),
        entity_type: Some("LLC".to_owned()),
        jurisdiction: "IL".to_owned(),
        state_file_number: Some(row.file_number.clone()),
        formation_date: row.organized_date.clone(),
        status: row.status_code.clone(),
        principal_address: records_office(&row),
        mailing_address: None,
        officers: None,
        registered_agent: agents.get(&row.file_number).cloned(),
        website: None,
        phone: None,
        category: None,
    };
    let company = companies::upsert(tx, new_company).await?;

    companies::record_source(
        tx,
        companies::NewCompanySource {
            company_id: company.id,
            source: "il_sos_llc".to_owned(),
            source_record_key: row.file_number.clone(),
            raw_file_path: raw_file_path.to_owned(),
            raw_row: serde_json::json!({
                "file_number": row.file_number,
                "status_code": row.status_code,
                "organized_date": row.organized_date,
                "name": name,
            }),
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

/// Import the LLC master file, joined with the name file (required) and the
/// agent file (optional) from the same directory. Idempotent on
/// `(il_sos_llc, file_number)`.
pub async fn import(
    pool: &SqlitePool,
    bucket: &Bucket,
    master_path: &Path,
    profile: Option<&TargetProfile>,
) -> anyhow::Result<ImportSummary> {
    let dir = master_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", master_path.display()))?;
    let name_path = dir.join("llcallnam.txt");
    if !name_path.exists() {
        anyhow::bail!(
            "expected {} alongside {} (the entity name file)",
            name_path.display(),
            master_path.display()
        );
    }
    let names = load_index(&name_path, parse_name_line)?;
    let agent_path = dir.join("llcallagt.txt");
    let agents = if agent_path.exists() {
        load_index(&agent_path, parse_agent_line)?
    } else {
        HashMap::new()
    };

    let today = Utc::now().format("%Y-%m-%d").to_string();
    let raw_file_path = bucket.store_source_file("il_sos_llc", &today, master_path)?;
    let file_name = master_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let keywords = ProfileKeywords::from(profile);
    let master_text = std::fs::read_to_string(master_path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", master_path.display()))?;

    let mut summary = ImportSummary::default();
    let mut tx = pool.begin().await?;
    for line in master_text.lines() {
        summary.seen += 1;
        match process_row(&mut tx, line, &names, &agents, &raw_file_path, &keywords).await? {
            RowOutcome::New { filtered } => {
                summary.new += 1;
                if filtered {
                    summary.filtered += 1;
                }
            }
            RowOutcome::Duplicate => summary.duplicate += 1,
            RowOutcome::NoNameOrUnparseable => summary.error += 1,
        }
    }

    coverage::record(
        &mut tx,
        NewCoverage {
            source: "il_sos_llc",
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
