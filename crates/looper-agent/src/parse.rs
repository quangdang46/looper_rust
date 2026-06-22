//! Completion marker parsing and native session ID extraction.

use crate::types::{CompletionParseStatus, CompletionPayload, COMPLETION_MARKER};

/// Parse the final completion line from combined stdout+stderr.
///
/// Scans in reverse line order for the last occurrence of `__LOOPER_RESULT__=`.
pub fn parse_completion(output: &str) -> (CompletionParseStatus, Option<CompletionPayload>) {
    // Search in reverse line order
    for line in output.lines().rev() {
        if let Some(pos) = line.find(COMPLETION_MARKER) {
            let json_str = &line[pos + COMPLETION_MARKER.len()..];
            match serde_json::from_str::<CompletionPayload>(json_str) {
                Ok(payload) => {
                    // Skip templates (placeholder summary text)
                    if payload.summary.trim() == "<one-sentence summary>" {
                        return (CompletionParseStatus::Missing, None);
                    }
                    return (CompletionParseStatus::Parsed, Some(payload));
                }
                Err(_) => {
                    return (CompletionParseStatus::InvalidJson, None);
                }
            }
        }
    }

    (CompletionParseStatus::Missing, None)
}

/// Extract native session ID from combined stdout+stderr.
///
/// Scans line-by-line for JSON keys: nativeSessionId, native_session_id, sessionId, session_id, chatId, chat_id.
/// Falls back to key:value / key=value extraction.
pub fn extract_native_session_id(output: &str) -> Option<String> {
    let session_keys = &[
        "nativeSessionId",
        "native_session_id",
        "sessionId",
        "session_id",
        "chatId",
        "chat_id",
    ];

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Try JSON parsing first
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(obj) = val.as_object() {
                for key in session_keys {
                    if let Some(session_val) = obj.get(*key) {
                        if let Some(s) = session_val.as_str() {
                            if !s.is_empty() {
                                return Some(s.to_string());
                            }
                        }
                    }
                }
            }
        }

        // Fallback: key:value / key=value extraction
        for key in session_keys {
            // Try key:value
            for sep in &[": ", ":"] {
                let pattern = format!("{}{}", key, sep);
                if let Some(pos) = trimmed.find(&pattern) {
                    let value = trimmed[pos + pattern.len()..].trim();
                    let value = value.trim_end_matches([',', '"', '\'', '}', ']']);
                    if !value.is_empty() && value.len() < 200 {
                        return Some(value.to_string());
                    }
                }
            }

            // Try key=value
            let pattern = format!("{}=", key);
            if let Some(pos) = trimmed.find(&pattern) {
                let value = trimmed[pos + pattern.len()..].trim();
                let value = value.trim_end_matches([',', '"', '\'', '}', ']']);
                if !value.is_empty() && value.len() < 200 {
                    // Check it looks like a session ID (alphanumeric-ish)
                    if !value.contains(' ') {
                        return Some(value.to_string());
                    }
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_completion_simple() {
        let output = format!(
            "some output\nfinal result\n{}",
            r#"__LOOPER_RESULT__={"summary":"done","artifacts":["file1.txt"]}"#
        );
        let (status, payload) = parse_completion(&output);
        assert_eq!(status, CompletionParseStatus::Parsed);
        let p = payload.unwrap();
        assert_eq!(p.summary, "done");
        assert_eq!(p.artifacts, vec!["file1.txt"]);
    }

    #[test]
    fn test_parse_completion_last_occurrence_wins() {
        let output = format!(
            "{}\nanother line\n{}",
            r#"__LOOPER_RESULT__={"summary":"first"}"#,
            r#"__LOOPER_RESULT__={"summary":"second"}"#
        );
        let (status, payload) = parse_completion(&output);
        assert_eq!(status, CompletionParseStatus::Parsed);
        assert_eq!(payload.unwrap().summary, "second");
    }

    #[test]
    fn test_parse_completion_invalid_json() {
        let output = "__LOOPER_RESULT__=not-json";
        let (status, _) = parse_completion(output);
        assert_eq!(status, CompletionParseStatus::InvalidJson);
    }

    #[test]
    fn test_parse_completion_missing() {
        let (status, _) = parse_completion("just some output\nno marker here");
        assert_eq!(status, CompletionParseStatus::Missing);
    }

    #[test]
    fn test_parse_completion_template_placeholder() {
        let output =
            r#"__LOOPER_RESULT__={"summary":"<one-sentence summary>"}"#;
        let (status, _) = parse_completion(output);
        assert_eq!(status, CompletionParseStatus::Missing);
    }

    #[test]
    fn test_extract_native_session_id_json_line() {
        let output = r#"{"session_id":"abc-123","status":"running"}"#;
        assert_eq!(
            extract_native_session_id(output),
            Some("abc-123".to_string())
        );
    }

    #[test]
    fn test_extract_native_session_id_key_value() {
        let output = "session_id: abc-123";
        assert_eq!(
            extract_native_session_id(output),
            Some("abc-123".to_string())
        );
    }

    #[test]
    fn test_extract_native_session_id_key_equals() {
        let output = "session_id=abc-123";
        assert_eq!(
            extract_native_session_id(output),
            Some("abc-123".to_string())
        );
    }

    #[test]
    fn test_extract_native_session_id_not_found() {
        assert_eq!(extract_native_session_id("just a normal line"), None);
    }

    #[test]
    fn test_extract_multiple_keys_order_preference() {
        let output = r#"{"nativeSessionId":"ns-1","sessionId":"ss-1"}"#;
        // nativeSessionId should be found first
        assert_eq!(
            extract_native_session_id(output),
            Some("ns-1".to_string())
        );
    }

    #[test]
    fn test_completion_with_all_fields() {
        let output = r#"__LOOPER_RESULT__={"summary":"fixed bug","artifacts":["src/main.rs"],"changedFiles":["src/main.rs"],"commits":["abc123"],"git_pr_lifecycle":{"pr_number":42}}"#;
        let (status, payload) = parse_completion(output);
        assert_eq!(status, CompletionParseStatus::Parsed);
        let p = payload.unwrap();
        assert_eq!(p.summary, "fixed bug");
        assert_eq!(p.commits, vec!["abc123"]);
        assert!(p.git_pr_lifecycle.is_some());
    }
}
