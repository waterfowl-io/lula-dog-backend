//! The cheap filter: no network, run at import time. Drops entities that are
//! clearly not worth enriching (inactive/dissolved/revoked, likely
//! registered-agent shells or holding companies) and scores the rest against
//! a target profile's keywords so enrichment can prioritize.

use serde::{Deserialize, Serialize};
use sqlx::Type;
use utoipa::ToSchema;

use super::normalize_name;

/// Status words that mean an entity is no longer operating. Matched as a
/// substring of the lowercased status text, so "Involuntarily Dissolved" and
/// "Revoked - failed annual report" both match.
const INACTIVE_STATUS_MARKERS: &[&str] = &[
    "dissolved",
    "revoked",
    "inactive",
    "terminated",
    "withdrawn",
    "expired",
    "forfeited",
];

/// Name fragments that suggest a registered-agent shell or a pure holding
/// company rather than an operating business. A small, hand-maintained
/// constant, deliberately: this is a heuristic, not a classifier.
const SHELL_NAME_MARKERS: &[&str] = &[
    "registered agent",
    "holdings",
    "holding company",
    "capital",
    "investments",
    "properties",
    "realty",
    "trust",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, ToSchema)]
#[serde(rename_all = "lowercase")]
#[sqlx(rename_all = "lowercase")]
pub enum FilterStatus {
    Pending,
    Passed,
    Filtered,
}

#[derive(Debug, Clone)]
pub struct FilterOutcome {
    pub status: FilterStatus,
    pub reason: Option<String>,
    pub pre_score: Option<i64>,
}

impl FilterOutcome {
    fn filtered(reason: impl Into<String>) -> Self {
        Self {
            status: FilterStatus::Filtered,
            reason: Some(reason.into()),
            pre_score: None,
        }
    }
}

/// Keyword weight in `pre_score`: every include keyword or industry word
/// found in the company name adds this much.
const KEYWORD_SCORE: i64 = 10;

/// `name`/`status`/`registered_agent` are the raw (not normalized) values on
/// the company. `include_keywords` and `industries` are scored the same way;
/// pass both from the target profile. All keyword lists are matched
/// case-insensitively as substrings of the company name.
pub fn apply_cheap_filter(
    name: &str,
    status: Option<&str>,
    registered_agent: Option<&str>,
    include_keywords: &[String],
    industries: &[String],
    exclude_keywords: &[String],
) -> FilterOutcome {
    if let Some(status) = status {
        let lower = status.to_lowercase();
        if INACTIVE_STATUS_MARKERS.iter().any(|m| lower.contains(m)) {
            return FilterOutcome::filtered(format!("inactive_status:{status}"));
        }
    }

    let normalized = normalize_name(name);
    if SHELL_NAME_MARKERS.iter().any(|m| normalized.contains(m)) {
        return FilterOutcome::filtered("likely_shell_or_holding_company");
    }
    if let Some(agent) = registered_agent
        && !agent.trim().is_empty()
        && normalized == normalize_name(agent)
    {
        return FilterOutcome::filtered("name_matches_registered_agent");
    }

    for keyword in exclude_keywords {
        let k = normalize_name(keyword);
        if !k.is_empty() && normalized.contains(&k) {
            return FilterOutcome::filtered(format!("excluded_keyword:{keyword}"));
        }
    }

    let matches = include_keywords
        .iter()
        .chain(industries)
        .filter(|kw| {
            let k = normalize_name(kw);
            !k.is_empty() && normalized.contains(&k)
        })
        .count();

    FilterOutcome {
        status: FilterStatus::Passed,
        reason: None,
        pre_score: Some(i64::try_from(matches).unwrap_or(i64::MAX) * KEYWORD_SCORE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kw(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    #[test]
    fn drops_inactive_entities() {
        let out = apply_cheap_filter("Acme Boxes LLC", Some("Revoked"), None, &[], &[], &[]);
        assert_eq!(out.status, FilterStatus::Filtered);
        assert_eq!(out.reason.as_deref(), Some("inactive_status:Revoked"));
    }

    #[test]
    fn drops_registered_agent_and_holding_shells() {
        for name in [
            "CT Corporation Registered Agent Services",
            "Acme Holdings LLC",
        ] {
            let out = apply_cheap_filter(name, Some("Active"), None, &[], &[], &[]);
            assert_eq!(out.status, FilterStatus::Filtered, "{name}");
        }
    }

    #[test]
    fn drops_entities_named_after_their_registered_agent() {
        let out = apply_cheap_filter(
            "Northwest Registered Agent LLC",
            Some("Active"),
            Some("Northwest Registered Agent LLC"),
            &[],
            &[],
            &[],
        );
        assert_eq!(out.status, FilterStatus::Filtered);
    }

    #[test]
    fn drops_excluded_keywords() {
        let out = apply_cheap_filter(
            "Acme Law Firm LLC",
            Some("Active"),
            None,
            &[],
            &[],
            &kw(&["law firm"]),
        );
        assert_eq!(out.status, FilterStatus::Filtered);
        assert_eq!(out.reason.as_deref(), Some("excluded_keyword:law firm"));
    }

    #[test]
    fn scores_keyword_matches_and_passes() {
        let out = apply_cheap_filter(
            "Acme Coffee Roasting Co",
            Some("Active"),
            None,
            &kw(&["coffee", "bakery"]),
            &kw(&["manufacturing"]),
            &[],
        );
        assert_eq!(out.status, FilterStatus::Passed);
        assert_eq!(out.pre_score, Some(10));
    }

    #[test]
    fn passes_with_zero_score_when_nothing_matches() {
        let out = apply_cheap_filter(
            "Acme Plain Co",
            Some("Active"),
            None,
            &kw(&["coffee"]),
            &[],
            &[],
        );
        assert_eq!(out.status, FilterStatus::Passed);
        assert_eq!(out.pre_score, Some(0));
    }
}
