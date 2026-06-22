use crate::types::DisclosureConfig;

/// A disclosure stamp marks generated content so users and tools can
/// identify AI-produced output.
///
/// Stamps are inserted into commit messages, PR descriptions/review
/// comments, and markdown output as configured.
#[derive(Debug, Clone)]
pub struct DisclosureStamp {
    pub text: String,
    pub format: StampFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StampFormat {
    /// `<!-- ai-generated: ... -->`
    MarkdownComment,
    /// `// ai-generated: ...`
    CodeComment,
    /// `(AI-generated: ...)`
    Trailer,
    /// Pure text (no wrapper)
    Text,
}

impl std::fmt::Display for StampFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MarkdownComment => write!(f, "markdown-comment"),
            Self::CodeComment => write!(f, "code-comment"),
            Self::Trailer => write!(f, "trailer"),
            Self::Text => write!(f, "text"),
        }
    }
}

/// Generate a disclosure stamp according to the given config.
pub fn generate_stamp(config: &DisclosureConfig) -> Option<DisclosureStamp> {
    if !config.stamp.enabled {
        return None;
    }

    let prefix = &config.stamp.prefix;
    let mut parts: Vec<String> = vec![prefix.clone()];

    if config.stamp.include_timestamp {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        parts.push(format!("ts={}", now));
    }

    if config.stamp.include_config_hash {
        // In a full implementation this would be a hash of the resolved config;
        // for now we use a placeholder.
        parts.push("config=<hash>".into());
    }

    let body = parts.join("; ");

    let text = match config.format {
        crate::enums::DisclosureFormat::Markdown => format!("<!-- {} -->", body),
        crate::enums::DisclosureFormat::Html => format!("<!-- {} -->", body),
        crate::enums::DisclosureFormat::Text => body,
    };

    Some(DisclosureStamp { text, format: StampFormat::Text })
}

/// Check whether the given text contains a disclosure stamp.
pub fn has_stamp(text: &str, config: &DisclosureConfig) -> bool {
    if let Some(stamp) = generate_stamp(config) {
        text.contains(&stamp.text)
            || text.contains(&format!("<!-- {}:", config.stamp.prefix))
    } else {
        false
    }
}

/// Remove disclosure stamps from the given text.
///
/// Returns the cleaned text and whether any stamp was removed.
pub fn strip_stamps(text: &str, config: &DisclosureConfig) -> (String, bool) {
    let mut removed = false;
    let mut result = text.to_string();

    if config.stamp.enabled {
        // Remove HTML/ Markdown comment stamps
        let comment_pattern = format!("<!-- {}:", config.stamp.prefix);
        if let Some(pos) = result.find(&comment_pattern) {
            if let Some(end) = result[pos..].find("-->") {
                result.replace_range(pos..=pos + end + 2, "");
                removed = true;
            }
        }

        // Remove trailer stamps (at end of lines)
        let trailer = format!("({}:", config.stamp.prefix);
        if let Some(pos) = result.rfind(&trailer) {
            if let Some(end) = result[pos..].find(')') {
                result.replace_range(pos..=pos + end + 1, "");
                removed = true;
            }
        }
    }

    (result.trim().to_string(), removed)
}

/// Check if the given text contains any protected phrases.
pub fn check_protected_phrases<'a>(text: &'a str, config: &'a DisclosureConfig) -> Vec<&'a str> {
    if config.protected_phrases.is_empty() {
        return vec![];
    }
    config
        .protected_phrases
        .iter()
        .filter(|phrase| {
            let lower_text = text.to_lowercase();
            lower_text.contains(&phrase.to_lowercase())
        })
        .map(|s| s.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::DisclosureConfig;

    fn test_config() -> DisclosureConfig {
        DisclosureConfig {
            stamp: crate::types::DisclosureStampConfig {
                enabled: true,
                prefix: "ai-generated".into(),
                include_timestamp: false,
                include_config_hash: false,
            },
            ..Default::default()
        }
    }

    #[test]
    fn test_generates_stamp_when_enabled() {
        let config = test_config();
        let stamp = generate_stamp(&config);
        assert!(stamp.is_some());
        assert!(stamp.unwrap().text.contains("ai-generated"));
    }

    #[test]
    fn test_no_stamp_when_disabled() {
        let mut config = test_config();
        config.stamp.enabled = false;
        assert!(generate_stamp(&config).is_none());
    }

    #[test]
    fn test_has_stamp_detects_markdown() {
        let config = test_config();
        let text = "some content <!-- ai-generated: text --> more";
        assert!(has_stamp(text, &config));
    }

    #[test]
    fn test_strip_removes_markdown_stamp() {
        let config = test_config();
        let text = "hello <!-- ai-generated: test --> world";
        let (cleaned, removed) = strip_stamps(text, &config);
        assert!(removed);
        assert!(!cleaned.contains("ai-generated"));
        assert!(cleaned.contains("hello"));
        assert!(cleaned.contains("world"));
    }

    #[test]
    fn test_strip_noop_without_stamp() {
        let config = test_config();
        let text = "hello world";
        let (cleaned, removed) = strip_stamps(text, &config);
        assert!(!removed);
        assert_eq!(cleaned, "hello world");
    }

    #[test]
    fn test_protected_phrases_found() {
        let mut config = test_config();
        config.protected_phrases = vec!["secret".into(), "confidential".into()];
        let found = check_protected_phrases("This contains secret info", &config);
        assert_eq!(found, vec!["secret"]);
    }

    #[test]
    fn test_protected_phrases_case_insensitive() {
        let mut config = test_config();
        config.protected_phrases = vec!["SECRET".into()];
        let found = check_protected_phrases("this has Secret info", &config);
        assert_eq!(found, vec!["SECRET"]);
    }

    #[test]
    fn test_protected_phrases_none() {
        let config = test_config();
        let found = check_protected_phrases("safe content", &config);
        assert!(found.is_empty());
    }
}
