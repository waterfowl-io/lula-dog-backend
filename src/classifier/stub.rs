//! A deterministic, network-free `Classifier`. Scores keyword matches
//! between the company name/crawled text and the target profile the same
//! way the cheap filter does; good enough to exercise `classify` end to end
//! offline, not a substitute for a real model.

use async_trait::async_trait;

use super::{Classification, ClassificationInput, Classifier, ClassifyError};
use crate::companies::normalize_name;

/// Keywords suggesting the site talks about shipping physical goods.
const SHIPPING_MARKERS: &[&str] = &[
    "ship",
    "shipping",
    "order",
    "warehouse",
    "packaging",
    "wholesale",
    "manufactur",
];

#[derive(Debug, Default, Clone, Copy)]
pub struct StubClassifier;

#[async_trait]
impl Classifier for StubClassifier {
    async fn classify(
        &self,
        input: ClassificationInput<'_>,
    ) -> Result<Classification, ClassifyError> {
        let haystack = normalize_name(&format!("{} {}", input.company.name, input.crawled_text));

        let matched: Vec<&String> = input
            .profile
            .include_keywords
            .0
            .iter()
            .chain(&input.profile.industries.0)
            .filter(|kw| {
                let k = normalize_name(kw);
                !k.is_empty() && haystack.contains(&k)
            })
            .collect();

        let fit_score = i64::try_from(matched.len().min(10) * 10).unwrap_or(100);
        let industry = matched.first().map(|s| (*s).clone());
        let fit_reason = if matched.is_empty() {
            Some("no profile keywords matched the name or crawled text".to_owned())
        } else {
            let names: Vec<&str> = matched.iter().map(|s| s.as_str()).collect();
            Some(format!("matched keywords: {}", names.join(", ")))
        };
        let ships_physical_goods = SHIPPING_MARKERS.iter().any(|m| haystack.contains(m));

        Ok(Classification {
            industry,
            products: Vec::new(),
            ships_physical_goods,
            size_signals: Vec::new(),
            contact_page_url: input.contact_hint.map(str::to_owned),
            fit_score,
            fit_reason,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target_profiles::{NewTargetProfile, TargetProfile};

    fn profile() -> TargetProfile {
        let new = NewTargetProfile {
            name: "test".to_owned(),
            industries: vec!["bakery".to_owned()],
            regions: vec![],
            size_min: None,
            size_max: None,
            size_notes: None,
            include_keywords: vec!["coffee".to_owned()],
            exclude_keywords: vec![],
            min_fit_score: Some(50),
        };
        TargetProfile {
            id: 1,
            name: new.name,
            industries: sqlx::types::Json(new.industries),
            regions: sqlx::types::Json(new.regions),
            size_min: new.size_min,
            size_max: new.size_max,
            size_notes: new.size_notes,
            include_keywords: sqlx::types::Json(new.include_keywords),
            exclude_keywords: sqlx::types::Json(new.exclude_keywords),
            min_fit_score: 50,
            created_at: "2026-01-01 00:00:00".to_owned(),
            updated_at: "2026-01-01 00:00:00".to_owned(),
        }
    }

    fn company(name: &str) -> crate::companies::Company {
        crate::companies::Company {
            id: 1,
            name: name.to_owned(),
            normalized_name: normalize_name(name),
            entity_type: None,
            jurisdiction: "IL".to_owned(),
            state_file_number: None,
            formation_date: None,
            status: None,
            principal_address: None,
            mailing_address: None,
            officers: None,
            registered_agent: None,
            website: None,
            domain: None,
            phone: None,
            category: None,
            filter_status: crate::companies::FilterStatus::Passed,
            filter_reason: None,
            pre_score: None,
            created_at: "2026-01-01 00:00:00".to_owned(),
            updated_at: "2026-01-01 00:00:00".to_owned(),
        }
    }

    #[tokio::test]
    async fn scores_matched_keywords_and_flags_shipping() -> Result<(), ClassifyError> {
        let profile = profile();
        let company = company("Acme Coffee Roasters");
        let got = StubClassifier
            .classify(ClassificationInput {
                company: &company,
                profile: &profile,
                crawled_text: "We ship nationwide from our warehouse.",
                contact_hint: Some("https://acme.example/contact"),
            })
            .await?;
        assert_eq!(got.industry.as_deref(), Some("coffee"));
        assert_eq!(got.fit_score, 10);
        assert!(got.ships_physical_goods);
        assert_eq!(
            got.contact_page_url.as_deref(),
            Some("https://acme.example/contact")
        );
        Ok(())
    }

    #[tokio::test]
    async fn no_match_is_a_zero_score() -> Result<(), ClassifyError> {
        let profile = profile();
        let company = company("Acme Plain Co");
        let got = StubClassifier
            .classify(ClassificationInput {
                company: &company,
                profile: &profile,
                crawled_text: "",
                contact_hint: None,
            })
            .await?;
        assert_eq!(got.fit_score, 0);
        assert_eq!(got.industry, None);
        Ok(())
    }
}
