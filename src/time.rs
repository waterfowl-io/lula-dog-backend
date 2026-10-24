//! Timestamp normalization.
//!
//! Every timestamp is stored as TEXT in SQLite's CURRENT_TIMESTAMP format,
//! `YYYY-MM-DD HH:MM:SS` in UTC, so the defaults the schema fills in and the
//! values the application writes sort together as plain strings.

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};

pub const FORMAT: &str = "%Y-%m-%d %H:%M:%S";

/// Parse a caller- or extractor-supplied timestamp and return it in the
/// storage format. Accepts RFC 3339 (`2026-10-14T09:30:00Z`, with any
/// offset), RFC 2822 (an email `Date:` header), a naive
/// `YYYY-MM-DD[ T]HH:MM[:SS]` taken as UTC, or a bare `YYYY-MM-DD` (midnight
/// UTC). Returns `None` for anything else.
pub fn normalize(input: &str) -> Option<String> {
    parse(input).map(|dt| format(&dt))
}

pub fn parse(input: &str) -> Option<DateTime<Utc>> {
    let s = input.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(dt) = DateTime::parse_from_rfc2822(s) {
        return Some(dt.with_timezone(&Utc));
    }
    for fmt in [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some(naive.and_utc());
        }
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|naive| naive.and_utc())
}

pub fn format(dt: &DateTime<Utc>) -> String {
    dt.format(FORMAT).to_string()
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn accepts_the_supported_shapes() {
        let cases = [
            ("2026-10-14T09:30:00Z", "2026-10-14 09:30:00"),
            ("2026-10-14T09:30:00-07:00", "2026-10-14 16:30:00"),
            ("Wed, 14 Oct 2026 09:30:00 -0700", "2026-10-14 16:30:00"),
            ("2026-10-14 09:30:00", "2026-10-14 09:30:00"),
            ("2026-10-14T09:30", "2026-10-14 09:30:00"),
            ("2026-10-14", "2026-10-14 00:00:00"),
            ("  2026-10-14  ", "2026-10-14 00:00:00"),
        ];
        for (input, want) in cases {
            assert_eq!(normalize(input).as_deref(), Some(want), "input {input:?}");
        }
    }

    #[test]
    fn rejects_garbage() {
        for input in ["", "next tuesday", "2026-13-01", "14/10/2026"] {
            assert_eq!(normalize(input), None, "input {input:?}");
        }
    }
}
