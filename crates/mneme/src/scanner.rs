//! Secret / PII scanner.
//!
//! Returns the first offending pattern category found in `content`,
//! or `None` if content is clean. Patterns are checked in declaration
//! order, so put higher-signal ones first.
//!
//! Add a new pattern by appending to `PATTERNS`. The list is a small
//! but high-signal starter; expand as needed for credentials you
//! care about.

const PATTERNS: &[(&str, &str)] = &[
    (r"AKIA[0-9A-Z]{16}", "AWS access key"),
    (r"sk-[A-Za-z0-9]{20,}", "OpenAI-style key"),
    (r"ghp_[A-Za-z0-9]{30,}", "GitHub PAT"),
    (r"xox[abp]-[A-Za-z0-9-]{10,}", "Slack token"),
    (r"AIza[0-9A-Za-z\-_]{35}", "Google API key"),
];

/// Returns the description of the first matched pattern, or `None`.
pub fn scan(content: &str) -> Option<&'static str> {
    for (pat, desc) in PATTERNS {
        if let Ok(re) = regex::Regex::new(pat) {
            if re.is_match(content) {
                return Some(desc);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_content_passes() {
        assert!(scan("just a normal string").is_none());
    }

    #[test]
    fn aws_key_detected() {
        assert!(scan("AKIAIOSFODNN7EXAMPLE").is_some());
    }

    #[test]
    fn github_pat_detected() {
        let pat = "ghp_".to_string() + &"a".repeat(36);
        assert!(scan(&pat).is_some());
    }
}
