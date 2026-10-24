//! The AI classification seam, same shape as `crate::extractor`: given a
//! company record, the target profile and its crawled site text, a
//! `Classifier` scores fit for packaging sales.
//!
//! Two impls: `StubClassifier` (deterministic keyword heuristics, the
//! default and what tests use) and `GrokClassifier` (xAI). Output is
//! untrusted: `Classification::sanitize` clamps or drops anything that
//! doesn't make sense before it is stored.

mod grok;
mod stub;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub use grok::GrokClassifier;
pub use stub::StubClassifier;

use crate::companies::Company;
use crate::target_profiles::TargetProfile;

pub struct ClassificationInput<'a> {
    pub company: &'a Company,
    pub profile: &'a TargetProfile,
    /// Text extracted from the crawled site, home page first. Empty when
    /// nothing was crawled.
    pub crawled_text: &'a str,
    /// A crawled page URL whose path looked like a contact page, if any;
    /// the classifier may use or confirm this rather than guess one from
    /// plain text.
    pub contact_hint: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Classification {
    pub industry: Option<String>,
    #[serde(default)]
    pub products: Vec<String>,
    pub ships_physical_goods: bool,
    #[serde(default)]
    pub size_signals: Vec<String>,
    pub contact_page_url: Option<String>,
    /// 0-100.
    pub fit_score: i64,
    pub fit_reason: Option<String>,
}

impl Classification {
    /// Clamp `fit_score` into 0..=100, drop blank list entries, and turn a
    /// blank `industry`/`contact_page_url` into `None`. Called on any
    /// output that did not originate from this process (i.e. an AI
    /// classifier), never needed for `StubClassifier`'s own output.
    #[must_use]
    pub fn sanitize(mut self) -> Self {
        self.fit_score = self.fit_score.clamp(0, 100);
        self.industry = self.industry.filter(|s| !s.trim().is_empty());
        self.contact_page_url = self.contact_page_url.filter(|s| !s.trim().is_empty());
        self.products.retain(|s| !s.trim().is_empty());
        self.size_signals.retain(|s| !s.trim().is_empty());
        self
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClassifyError {
    #[error("classifier request failed: {0}")]
    Request(String),
    #[error("classifier returned HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error("classifier returned malformed output: {0}")]
    Malformed(String),
}

#[async_trait]
pub trait Classifier: Send + Sync {
    async fn classify(
        &self,
        input: ClassificationInput<'_>,
    ) -> Result<Classification, ClassifyError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_clamps_and_drops_blanks() {
        let dirty = Classification {
            industry: Some("   ".to_owned()),
            products: vec!["Boxes".to_owned(), "  ".to_owned()],
            ships_physical_goods: true,
            size_signals: vec![String::new(), "50 employees".to_owned()],
            contact_page_url: Some(String::new()),
            fit_score: 140,
            fit_reason: Some("looks good".to_owned()),
        };
        let clean = dirty.sanitize();
        assert_eq!(clean.industry, None);
        assert_eq!(clean.products, vec!["Boxes".to_owned()]);
        assert_eq!(clean.size_signals, vec!["50 employees".to_owned()]);
        assert_eq!(clean.contact_page_url, None);
        assert_eq!(clean.fit_score, 100);
    }

    #[test]
    fn sanitize_clamps_negative_scores_too() {
        let dirty = Classification {
            industry: None,
            products: vec![],
            ships_physical_goods: false,
            size_signals: vec![],
            contact_page_url: None,
            fit_score: -5,
            fit_reason: None,
        };
        assert_eq!(dirty.sanitize().fit_score, 0);
    }
}
