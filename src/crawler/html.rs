//! Minimal, dependency-free HTML handling: strip tags down to visible text,
//! and pull `href` values out for link discovery. Byte-offset scanning is
//! always done through `.get()`, never indexing, so malformed markup or an
//! offset that lands off a UTF-8 boundary yields nothing instead of a panic.

/// Visible text only: `<script>`/`<style>` contents are dropped entirely,
/// every other tag is removed, a handful of named entities are decoded, and
/// whitespace is collapsed.
pub fn strip_tags(html: &str) -> String {
    let without_scripts = remove_blocks(html, "<script", "</script>");
    let without_styles = remove_blocks(&without_scripts, "<style", "</style>");
    let mut out = String::with_capacity(without_styles.len());
    // A space at every tag boundary, so adjacent block elements (`</h1><p>`)
    // don't run two words together; the whitespace collapse below cleans up.
    let mut in_tag = false;
    for ch in without_styles.chars() {
        match ch {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if in_tag => {}
            other => out.push(other),
        }
    }
    decode_entities(&out)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn decode_entities(text: &str) -> String {
    text.replace("&nbsp;", " ")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Remove every `open ... close` span, case-insensitively. A span with no
/// matching `close` removes to the end of the string.
fn remove_blocks(html: &str, open: &str, close: &str) -> String {
    let lower = html.to_lowercase();
    let open = open.to_lowercase();
    let close = close.to_lowercase();
    let mut out = String::new();
    let mut pos = 0usize;
    loop {
        let Some(start_rel) = lower.get(pos..).and_then(|rest| rest.find(&open)) else {
            out.push_str(html.get(pos..).unwrap_or(""));
            break;
        };
        let start = pos + start_rel;
        out.push_str(html.get(pos..start).unwrap_or(""));
        let after_open = start + open.len();
        match lower.get(after_open..).and_then(|rest| rest.find(&close)) {
            Some(close_rel) => pos = after_open + close_rel + close.len(),
            None => break,
        }
    }
    out
}

/// Every `href="..."` / `href='...'` value, in document order, duplicates
/// included (the caller dedupes).
pub fn extract_links(html: &str) -> Vec<String> {
    let lower = html.to_lowercase();
    let bytes = lower.as_bytes();
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(rel) = lower.get(pos..).and_then(|rest| rest.find("href")) {
        let idx = pos + rel;
        let mut j = idx + 4;
        while bytes.get(j).is_some_and(u8::is_ascii_whitespace) {
            j += 1;
        }
        if bytes.get(j) != Some(&b'=') {
            pos = idx + 4;
            continue;
        }
        j += 1;
        while bytes.get(j).is_some_and(u8::is_ascii_whitespace) {
            j += 1;
        }
        let Some(&quote) = bytes.get(j) else {
            pos = idx + 4;
            continue;
        };
        if quote != b'"' && quote != b'\'' {
            pos = idx + 4;
            continue;
        }
        let start = j + 1;
        let Some(end_rel) = bytes
            .get(start..)
            .and_then(|rest| rest.iter().position(|b| *b == quote))
        else {
            pos = idx + 4;
            continue;
        };
        let end = start + end_rel;
        if let Some(value) = html.get(start..end) {
            out.push(value.to_owned());
        }
        pos = end + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_tags_scripts_styles_and_decodes_entities() {
        let html = concat!(
            "<html><head><style>body{color:red}</style></head><body>",
            "<script>alert('hi')</script>",
            "<h1>Welcome &amp; Co</h1><p>We ship &quot;great&quot; packaging.</p>",
            "</body></html>",
        );
        assert_eq!(
            strip_tags(html),
            "Welcome & Co We ship \"great\" packaging."
        );
    }

    #[test]
    fn strips_tags_with_no_script_or_style() {
        assert_eq!(strip_tags("<p>Hello <b>world</b></p>"), "Hello world");
    }

    #[test]
    fn extracts_href_values_with_either_quote_style() {
        let html = r#"<a href="/about">About</a> <a href='/contact'>Contact</a>"#;
        assert_eq!(extract_links(html), vec!["/about", "/contact"]);
    }

    #[test]
    fn ignores_missing_or_malformed_href() {
        assert_eq!(extract_links("<a>no href</a>"), Vec::<String>::new());
        assert_eq!(
            extract_links("<a href=unquoted>bad</a>"),
            Vec::<String>::new()
        );
    }
}
