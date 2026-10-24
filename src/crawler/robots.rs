//! A deliberately simple `robots.txt`: only `User-agent: *` groups and
//! `Disallow` are honored (no `Allow` overrides, no other user-agent
//! groups). Good enough to respect an opt-out; not a full implementation.

/// Disallowed path prefixes for `User-agent: *`. An empty `Disallow` value
/// (meaning "allow everything") is never added to the list.
pub fn parse(text: &str) -> Vec<String> {
    let mut rules = Vec::new();
    let mut in_star_block = false;
    for raw_line in text.lines() {
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim().to_lowercase().as_str() {
            "user-agent" => in_star_block = value == "*",
            "disallow" if in_star_block && !value.is_empty() => rules.push(value.to_owned()),
            _ => {}
        }
    }
    rules
}

pub fn allowed(path: &str, disallow: &[String]) -> bool {
    !disallow.iter().any(|rule| path.starts_with(rule.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_disallow_rules_for_the_wildcard_agent() {
        let text = "User-agent: Googlebot\nDisallow: /private\n\nUser-agent: *\nDisallow: /admin\nDisallow: /tmp\n";
        let rules = parse(text);
        assert_eq!(rules, vec!["/admin".to_owned(), "/tmp".to_owned()]);
        assert!(!allowed("/admin/x", &rules));
        assert!(allowed("/about", &rules));
    }

    #[test]
    fn an_empty_disallow_means_allow_everything() {
        let rules = parse("User-agent: *\nDisallow:\n");
        assert_eq!(rules, Vec::<String>::new());
        assert!(allowed("/anything", &rules));
    }

    #[test]
    fn no_wildcard_group_means_no_restrictions() {
        let rules = parse("User-agent: Googlebot\nDisallow: /private\n");
        assert_eq!(rules, Vec::<String>::new());
    }
}
