//! Importers turn raw source files into `companies` rows: archive the file
//! in the bucket unchanged, parse it, dedupe/merge against existing
//! companies, run the cheap filter once per company, and leave a
//! `coverage_log` entry with counts.

pub mod bizapedia;

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
