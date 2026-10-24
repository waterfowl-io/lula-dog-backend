//! A deterministic, network-free extractor. Line-oriented heuristics, good
//! enough to exercise the paste flow end to end and to try the API without a
//! key; not a substitute for the real model.
//!
//! What it recognizes (case-insensitive prefixes, one per line):
//! - `Subject: ...` becomes the summary (else the first non-header line).
//! - `From: Name <email>` sets contact_name and contact_email (first one wins).
//! - `Company: ...` sets company.
//! - `Next step: ...` sets next_step.
//! - `TODO: title [by YYYY-MM-DD]` suggests a task.
//! - `Date: <RFC 2822>` lines are message dates; the latest becomes
//!   latest_date. With no `Date:` line, the latest `YYYY-MM-DD` token is used.
//! - The first `$1,234.56` amount sets value_cents.
//! - Stage keywords: "not moving forward" / "went with another" (lost),
//!   "purchase order" / "po attached" (won), "quote" / "proposal" (proposal).

use async_trait::async_trait;

use super::{ExtractError, Extraction, ExtractionInput, Extractor, SuggestedTask};
use crate::time;

const SUMMARY_MAX_CHARS: usize = 160;

#[derive(Debug, Default, Clone, Copy)]
pub struct StubExtractor;

#[async_trait]
impl Extractor for StubExtractor {
    async fn extract(&self, input: ExtractionInput<'_>) -> Result<Extraction, ExtractError> {
        Ok(extract(input.text))
    }
}

pub fn extract(text: &str) -> Extraction {
    let mut out = Extraction::default();
    let mut subject = None;
    let mut first_body_line = None;
    let mut latest_header: Option<String> = None;
    let mut latest_token: Option<String> = None;
    let mut stage: Option<&'static str> = None;

    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(v) = after_prefix(line, "subject:") {
            subject.get_or_insert_with(|| v.to_owned());
        } else if let Some(v) = after_prefix(line, "from:") {
            if out.contact_email.is_none() {
                let (name, email) = parse_from(v);
                out.contact_name = name;
                out.contact_email = email;
                if out.contact_name.is_some() || out.contact_email.is_some() {
                    out.evidence.contact_name = Some(line.to_owned());
                    out.evidence.contact_email = Some(line.to_owned());
                }
            }
        } else if let Some(v) = after_prefix(line, "company:") {
            if out.company.is_none() {
                out.company = Some(v.to_owned());
                out.evidence.company = Some(line.to_owned());
            }
        } else if let Some(v) = after_prefix(line, "next step:") {
            if out.next_step.is_none() {
                out.next_step = Some(v.to_owned());
                out.evidence.next_step = Some(line.to_owned());
            }
        } else if let Some(v) = after_prefix(line, "todo:") {
            out.tasks.push(parse_todo(v));
        } else if let Some(v) = after_prefix(line, "date:") {
            if let Some(d) = time::normalize(v) {
                latest_header = Some(max_date(latest_header, d));
            }
        } else if first_body_line.is_none() && !is_header(line) {
            first_body_line = Some(line.to_owned());
        }

        if stage.is_none()
            && let Some(s) = stage_from_keywords(&line.to_lowercase())
        {
            stage = Some(s);
            out.evidence.stage = Some(line.to_owned());
        }

        for token in line.split(|c: char| c.is_whitespace() || c == ',' || c == '(' || c == ')') {
            let token = token.trim_end_matches(['.', ';', ':', '?', '!']);
            if token.len() == 10
                && let Some(d) = time::normalize(token)
            {
                latest_token = Some(max_date(latest_token, d));
            }
        }
        if out.value_cents.is_none()
            && let Some(cents) = parse_money(line)
        {
            out.value_cents = Some(cents);
            out.evidence.value_cents = Some(line.to_owned());
        }
    }

    out.summary = subject
        .or(first_body_line)
        .map(|s| s.chars().take(SUMMARY_MAX_CHARS).collect());
    out.stage = stage.map(str::to_owned);
    out.latest_date = latest_header.or(latest_token);
    out
}

fn after_prefix<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let head = line.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) {
        line.get(prefix.len()..)
            .map(str::trim)
            .filter(|v| !v.is_empty())
    } else {
        None
    }
}

fn is_header(line: &str) -> bool {
    ["to:", "cc:", "sent:", ">"]
        .iter()
        .any(|p| line.to_ascii_lowercase().starts_with(p))
}

/// `Dana Ortiz <dana@example.com>` or a bare address.
fn parse_from(v: &str) -> (Option<String>, Option<String>) {
    if let Some((name, rest)) = v.split_once('<') {
        let email = rest.split_once('>').map(|(e, _)| e.trim().to_owned());
        let name = name.trim().trim_matches('"').trim();
        let name = (!name.is_empty()).then(|| name.to_owned());
        (name, email.filter(|e| e.contains('@')))
    } else if v.contains('@') {
        (None, Some(v.to_owned()))
    } else {
        (Some(v.to_owned()), None)
    }
}

/// `Send revised quote by 2026-10-30`.
fn parse_todo(v: &str) -> SuggestedTask {
    if let Some((title, due)) = v.rsplit_once(" by ")
        && time::normalize(due).is_some()
    {
        return SuggestedTask {
            title: title.trim().to_owned(),
            due_at: Some(due.trim().to_owned()),
        };
    }
    SuggestedTask {
        title: v.to_owned(),
        due_at: None,
    }
}

fn max_date(current: Option<String>, candidate: String) -> String {
    // Normalized timestamps compare correctly as strings.
    match current {
        Some(c) if c >= candidate => c,
        _ => candidate,
    }
}

/// The first `$12,500` or `$12,500.00` on the line, in cents.
fn parse_money(line: &str) -> Option<i64> {
    let (_, rest) = line.split_once('$')?;
    let amount: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',' || *c == '.')
        .filter(|c| *c != ',')
        .collect();
    let amount = amount.trim_end_matches('.');
    let (dollars, cents) = match amount.split_once('.') {
        Some((d, c)) if c.len() == 2 => (d, c.parse::<i64>().ok()?),
        Some(_) => return None,
        None => (amount, 0),
    };
    dollars
        .parse::<i64>()
        .ok()?
        .checked_mul(100)?
        .checked_add(cents)
}

fn stage_from_keywords(lower: &str) -> Option<&'static str> {
    let any = |words: &[&str]| words.iter().any(|w| lower.contains(w));
    if any(&["not moving forward", "went with another"]) {
        Some("lost")
    } else if any(&["purchase order", "po attached"]) {
        Some("won")
    } else if any(&["quote", "proposal"]) {
        Some("proposal")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THREAD: &str = "\
From: Dana Ortiz <dana@example.com>
Date: Wed, 14 Oct 2026 09:30:00 -0700
Subject: Re: 12x10x8 RSC quote

Thanks for the quote. Pricing at $12,450.00 for 5,000 units works.
Can you hold that through 2026-10-20?
Next step: Send revised quote with double-wall option
TODO: Send double-wall pricing by 2026-10-17
TODO: Call Dana about pallet count
";

    #[test]
    fn extracts_the_documented_signals() {
        let got = extract(THREAD);
        assert_eq!(got.summary.as_deref(), Some("Re: 12x10x8 RSC quote"));
        assert_eq!(got.contact_name.as_deref(), Some("Dana Ortiz"));
        assert_eq!(got.contact_email.as_deref(), Some("dana@example.com"));
        assert_eq!(got.value_cents, Some(1_245_000));
        assert_eq!(got.stage.as_deref(), Some("proposal"));
        assert_eq!(
            got.next_step.as_deref(),
            Some("Send revised quote with double-wall option")
        );
        assert_eq!(got.latest_date.as_deref(), Some("2026-10-14 16:30:00"));
        assert_eq!(
            extract("Spoke 2026-10-02, follow up 2026-10-09.")
                .latest_date
                .as_deref(),
            Some("2026-10-09 00:00:00")
        );
        assert_eq!(
            got.tasks,
            vec![
                SuggestedTask {
                    title: "Send double-wall pricing".to_owned(),
                    due_at: Some("2026-10-17".to_owned()),
                },
                SuggestedTask {
                    title: "Call Dana about pallet count".to_owned(),
                    due_at: None,
                },
            ]
        );
    }

    #[test]
    fn captures_an_evidence_quote_per_field_it_sets() {
        let got = extract(THREAD);
        assert_eq!(
            got.evidence.contact_name.as_deref(),
            Some("From: Dana Ortiz <dana@example.com>")
        );
        assert_eq!(got.evidence.contact_name, got.evidence.contact_email);
        assert_eq!(
            got.evidence.value_cents.as_deref(),
            Some("Thanks for the quote. Pricing at $12,450.00 for 5,000 units works.")
        );
        assert_eq!(
            got.evidence.stage.as_deref(),
            Some("Subject: Re: 12x10x8 RSC quote")
        );
        assert_eq!(
            got.evidence.next_step.as_deref(),
            Some("Next step: Send revised quote with double-wall option")
        );
        // No `Company:` line in the thread, so no company evidence either.
        assert_eq!(got.company, None);
        assert_eq!(got.evidence.company, None);
    }

    #[test]
    fn is_deterministic_and_empty_on_plain_text() {
        let got = extract("just a note");
        assert_eq!(got, extract("just a note"));
        assert_eq!(got.summary.as_deref(), Some("just a note"));
        assert_eq!(got.value_cents, None);
        assert_eq!(got.latest_date, None);
        assert_eq!(got.tasks.len(), 0);
    }

    #[test]
    fn money_parsing() {
        assert_eq!(parse_money("about $800 total"), Some(80_000));
        assert_eq!(parse_money("$1,200.50."), Some(120_050));
        assert_eq!(parse_money("$12.5"), None);
        assert_eq!(parse_money("no amount"), None);
    }
}
