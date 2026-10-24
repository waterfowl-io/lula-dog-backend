//! Importers turn raw source files into `companies` rows: archive the file
//! in the bucket unchanged, parse it, dedupe/merge against existing
//! companies, run the cheap filter once per company, and leave a
//! `coverage_log` entry with counts.

pub mod bizapedia;
pub mod il_sos_llc;

use crate::target_profiles::TargetProfile;

/// Lowercase, trim, collapse internal whitespace. Used to match a CSV header
/// (or a fixed-width field label) against a configured alias regardless of
/// case or spacing.
pub fn normalize_header(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Keyword lists scored against a company name by the cheap filter, taken
/// from the active target profile (empty when there is none yet).
#[derive(Debug, Clone, Default)]
pub struct ProfileKeywords {
    pub include: Vec<String>,
    pub industries: Vec<String>,
    pub exclude: Vec<String>,
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

#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct ImportSummary {
    pub seen: i64,
    pub new: i64,
    pub duplicate: i64,
    pub filtered: i64,
    pub error: i64,
}

impl From<ImportSummary> for crate::coverage::Counts {
    fn from(s: ImportSummary) -> Self {
        Self {
            seen: s.seen,
            new: s.new,
            duplicate: s.duplicate,
            filtered: s.filtered,
            error: s.error,
        }
    }
}
