//! Internal helper functions for the GitHub gateway.

use std::collections::HashMap;

use crate::types::{
    CommentInfo, DependencyIssue, GitHubUser, IssueRepository, LabelInitSummary, PullRequestAutoMerge, ReviewComment,
    ReviewIdempotencyMarker, ReviewThreadComment,
};
use serde_json::Value;

// ---------------------------------------------------------------------------
// JSON parsing
// ---------------------------------------------------------------------------

/// Decode a JSON string into a map.
pub fn decode_json_object(value: &str) -> Result<HashMap<String, Value>, serde_json::Error> {
    serde_json::from_str(value)
}

/// Decode a JSON string into an array of maps.
pub fn decode_json_array(value: &str) -> Result<Vec<HashMap<String, Value>>, serde_json::Error> {
    let arr: Vec<Value> = serde_json::from_str(value)?;
    Ok(arr
        .into_iter()
        .filter_map(|v| v.as_object().map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect()))
        .collect())
}

/// Decode a JSON string that may be a single array or paginated (slurped) arrays.
pub fn decode_json_array_or_pages(value: &str) -> Result<Vec<HashMap<String, Value>>, serde_json::Error> {
    let v: Value = serde_json::from_str(value)?;
    match v {
        Value::Array(arr) => {
            let mut result = Vec::new();
            for item in arr {
                if let Some(map) = item.as_object() {
                    let m: HashMap<String, Value> = map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                    result.push(m);
                } else if item.is_array() {
                    if let Some(sub_arr) = item.as_array() {
                        for sub in sub_arr {
                            if let Some(sub_map) = sub.as_object() {
                                let m: HashMap<String, Value> =
                                    sub_map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                                result.push(m);
                            }
                        }
                    }
                }
            }
            Ok(result)
        }
        _ => Ok(vec![]),
    }
}

// ---------------------------------------------------------------------------
// Field extraction from raw JSON
// ---------------------------------------------------------------------------

pub fn as_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        _ => value.to_string(),
    }
}

pub fn as_bool(value: &Value) -> bool {
    match value {
        Value::Bool(b) => *b,
        Value::String(s) => s == "true" || s == "yes" || s == "1",
        Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
        Value::Null => false,
        _ => false,
    }
}

pub fn as_i64(value: &Value) -> i64 {
    match value {
        Value::Number(n) => n.as_i64().unwrap_or(0),
        Value::String(s) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

pub fn bool_ptr_from_value(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(b) => Some(*b),
        Value::Null => None,
        _ => None,
    }
}

pub fn to_object_slice(value: &Value) -> Vec<HashMap<String, Value>> {
    match value {
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_object().map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect()))
            .collect(),
        _ => vec![],
    }
}

/// Navigate a nested JSON object by path segments.
pub fn nested_string(value: &HashMap<String, Value>, path: &[&str]) -> String {
    let mut current: Option<&Value> = None;
    for (i, key) in path.iter().enumerate() {
        if i == 0 {
            current = value.get(*key);
        } else if let Some(Value::Object(map)) = current {
            current = map.get(*key);
        } else {
            return String::new();
        }
    }
    current.map(as_string).unwrap_or_default()
}

pub fn first_non_empty(values: &[&str]) -> String {
    values.iter().find(|s| !s.is_empty()).map(|s| s.to_string()).unwrap_or_default()
}

pub fn first_non_nil<'a>(values: &'a [&'a Value]) -> Option<&'a Value> {
    values.iter().find(|v| !v.is_null()).copied()
}

// ---------------------------------------------------------------------------
// Author / label extraction
// ---------------------------------------------------------------------------

pub fn extract_author(value: &Value) -> String {
    match value {
        Value::Object(map) => map.get("login").or_else(|| map.get("name")).map(as_string).unwrap_or_default(),
        _ => String::new(),
    }
}

pub fn extract_oid(value: &Value) -> String {
    match value {
        Value::Object(map) => map.get("oid").map(as_string).unwrap_or_default(),
        _ => String::new(),
    }
}

pub fn extract_label_names(value: &Value) -> Vec<String> {
    let arr = match value {
        Value::Array(a) => a,
        _ => return vec![],
    };
    arr.iter()
        .filter_map(|v| v.as_object().and_then(|m| m.get("name")).map(as_string).filter(|s| !s.is_empty()))
        .collect()
}

pub fn extract_label_names_from_connection(value: &Value) -> Vec<String> {
    let nodes = match value {
        Value::Object(map) => map.get("nodes"),
        _ => return vec![],
    };
    match nodes {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_object().and_then(|m| m.get("name")).map(as_string).filter(|s| !s.is_empty()))
            .collect(),
        _ => vec![],
    }
}

pub fn extract_review_request_logins(value: &Value) -> Vec<String> {
    match value {
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| {
                v.as_object()
                    .and_then(|m| m.get("requestedReviewer"))
                    .and_then(|r| r.as_object())
                    .and_then(|r| r.get("login"))
                    .map(as_string)
            })
            .collect(),
        _ => vec![],
    }
}

pub fn extract_review_request_users(value: &Value) -> Vec<GitHubUser> {
    match value {
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| {
                v.as_object().and_then(|m| m.get("requestedReviewer")).and_then(|r| r.as_object()).map(|r| GitHubUser {
                    login: r.get("login").map(as_string).unwrap_or_default(),
                    id: r.get("id").map(as_i64).unwrap_or(0),
                })
            })
            .collect(),
        _ => vec![],
    }
}

pub fn extract_actor_logins(value: &Value) -> Vec<String> {
    match value {
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| {
                v.as_object()
                    .and_then(|m| m.get("actor"))
                    .and_then(|a| a.as_object())
                    .and_then(|a| a.get("login"))
                    .map(as_string)
            })
            .collect(),
        _ => vec![],
    }
}

pub fn extract_actor_users(value: &Value) -> Vec<GitHubUser> {
    match value {
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| {
                v.as_object().and_then(|m| {
                    let actor = m.get("actor")?;
                    let obj = actor.as_object()?;
                    Some(GitHubUser {
                        login: obj.get("login").map(as_string).unwrap_or_default(),
                        id: obj.get("id").map(as_i64).unwrap_or(0),
                    })
                })
            })
            .collect(),
        _ => vec![],
    }
}

pub fn extract_comment_infos(value: &Value) -> Vec<CommentInfo> {
    match value {
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| {
                v.as_object().map(|m| CommentInfo {
                    id: m.get("id").map(as_i64).unwrap_or(0),
                    author: extract_author(&m.get("author").cloned().unwrap_or(Value::Null)),
                    author_association: m.get("authorAssociation").map(as_string).unwrap_or_default(),
                    body: m.get("body").map(as_string).unwrap_or_default(),
                    created_at: m.get("createdAt").map(as_string).unwrap_or_default(),
                    updated_at: m.get("updatedAt").map(as_string).unwrap_or_default(),
                    url: m.get("url").map(as_string).unwrap_or_default(),
                })
            })
            .collect(),
        _ => vec![],
    }
}

pub fn extract_dependency_issue(value: &HashMap<String, Value>, default_repo: &str) -> DependencyIssue {
    DependencyIssue {
        id: value.get("id").map(as_i64).unwrap_or(0),
        number: value.get("number").map(as_i64).unwrap_or(0),
        title: value.get("title").map(as_string).unwrap_or_default(),
        url: value.get("url").map(as_string).unwrap_or_default(),
        html_url: value.get("htmlUrl").map(as_string).unwrap_or_default(),
        repository_url: value.get("repositoryUrl").map(as_string).unwrap_or_default(),
        state: value.get("state").map(as_string).unwrap_or_default(),
        state_reason: value.get("stateReason").map(as_string).unwrap_or_default(),
        repository: value.get("repository").map(extract_issue_repository).unwrap_or_else(|| IssueRepository {
            name: String::new(),
            full_name: default_repo.to_string(),
            url: String::new(),
            html_url: String::new(),
        }),
    }
}

pub fn extract_issue_repository(value: &Value) -> IssueRepository {
    match value {
        Value::Object(map) => IssueRepository {
            name: map.get("name").map(as_string).unwrap_or_default(),
            full_name: map.get("fullName").map(as_string).unwrap_or_default(),
            url: map.get("url").map(as_string).unwrap_or_default(),
            html_url: map.get("htmlUrl").map(as_string).unwrap_or_default(),
        },
        _ => IssueRepository {
            name: String::new(),
            full_name: String::new(),
            url: String::new(),
            html_url: String::new(),
        },
    }
}

pub fn extract_auto_merge(value: &Value) -> Option<PullRequestAutoMerge> {
    match value {
        Value::Object(map) => {
            let enabled_by =
                map.get("enabledBy").and_then(|v| v.as_object()).and_then(|m| m.get("login")).map(as_string)?;
            let merge_method = map.get("mergeMethod").map(as_string)?;
            Some(PullRequestAutoMerge { enabled_by, merge_method })
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Repository helpers
// ---------------------------------------------------------------------------

/// Parse `"owner/repo"` into `(owner, name)`.
pub fn parse_repo(repo: &str) -> Result<(String, String), String> {
    let parts: Vec<&str> = repo.split('/').collect();
    if parts.len() >= 2 && !parts[0].is_empty() && !parts[1].is_empty() {
        Ok((parts[0].to_string(), parts[1].to_string()))
    } else {
        Err(format!("invalid repo format: {}", repo))
    }
}

/// Split a repo string into (hostname_or_empty, repo).
pub fn split_repo_hostname(repo: &str) -> (String, String) {
    if repo.contains('/') && !repo.contains("github.com") {
        // Assume first part is hostname
        if let Some((host, rest)) = repo.split_once('/') {
            if rest.contains('/') {
                (host.to_string(), rest.to_string())
            } else {
                (String::new(), repo.to_string())
            }
        } else {
            (String::new(), repo.to_string())
        }
    } else {
        (String::new(), repo.to_string())
    }
}

/// Split a `"owner/name"` into `(owner, name)`.
pub fn split_repo_owner_name(repo: &str) -> (String, String) {
    if let Some((owner, name)) = repo.split_once('/') {
        (owner.to_string(), name.to_string())
    } else {
        (String::new(), repo.to_string())
    }
}

/// Return a host-qualified repo name from name-with-owner and repo URL.
pub fn host_qualified_repo(name_with_owner: &str, repo_url: &str) -> String {
    // If the URL has a hostname prefix, include it
    if let Some(rest) = repo_url.strip_prefix("https://") {
        if let Some((host, _)) = rest.split_once('/') {
            if host != "github.com" && host != "api.github.com" {
                return format!("{}/{}", host, name_with_owner);
            }
        }
    }
    name_with_owner.to_string()
}

/// Validate that a string looks like a valid GitHub repo slug.
pub fn validate_github_repo_slug(repo: &str) -> Result<(), String> {
    if repo.is_empty() {
        return Err("repo slug is empty".into());
    }
    let parts: Vec<&str> = repo.split('/').collect();
    if parts.len() < 2 {
        return Err(format!("repo slug must be owner/name, got: {}", repo));
    }
    for part in &parts {
        if part.is_empty() {
            return Err(format!("repo slug has empty segment: {}", repo));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Review marker parsing
// ---------------------------------------------------------------------------

/// Find a review idempotency marker in a review body.
pub fn find_review_idempotency_marker(body: &str, marker: &str) -> Option<ReviewIdempotencyMarker> {
    let markers = parse_review_idempotency_markers(body);
    markers.into_iter().find(|m| m.id == marker)
}

/// Parse all review idempotency markers from a body.
///
/// Format: `<!-- looper-review:id:outcome:head -->` (hidden HTML comment)
pub fn parse_review_idempotency_markers(body: &str) -> Vec<ReviewIdempotencyMarker> {
    let mut results = Vec::new();
    // Pattern: <!-- looper-review:<id>:<outcome>:<head> -->
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("<!-- looper-review:") && trimmed.ends_with("-->") {
            let inner = trimmed.trim_start_matches("<!-- looper-review:").trim_end_matches("-->");
            let parts: Vec<&str> = inner.splitn(3, ':').collect();
            if parts.len() == 3 {
                results.push(ReviewIdempotencyMarker {
                    id: parts[0].to_string(),
                    outcome: parts[1].to_string(),
                    head: parts[2].to_string(),
                });
            }
        }
    }
    results
}

/// Map a review state to the corresponding review event.
pub fn review_event_from_state(state: &str) -> &str {
    match state {
        "APPROVED" => "APPROVE",
        "COMMENTED" => "COMMENT",
        "CHANGES_REQUESTED" => "REQUEST_CHANGES",
        _ => state,
    }
}

/// Check if a review event is in the allowed list.
pub fn review_event_allowed(event: &str, allowed: &[String]) -> bool {
    allowed.iter().any(|a| a == event)
}

// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

/// Returns the default limit (minimum 30).
pub fn default_limit(limit: i32) -> i32 {
    if limit < 30 {
        30
    } else {
        limit
    }
}

/// Generate a random ID (UUID v4).
pub fn random_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Encode a string for use in a URI component.
pub fn encode_uri_component(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(byte as char);
            }
            b' ' => result.push_str("%20"),
            _ => {
                result.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    result
}

pub fn string_ptr(value: String) -> Option<String> {
    Some(value)
}

pub fn string_ptr_if_not_empty(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

pub fn empty_to_nil(value: &str) -> Option<&str> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

pub fn value_or<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.is_empty() {
        fallback
    } else {
        value
    }
}

/// Deduplicate a list of strings while preserving order.
pub fn unique_strings(values: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::with_capacity(values.len());
    for v in values {
        if seen.insert(v.clone()) {
            result.push(v.clone());
        }
    }
    result
}

/// Summarize check runs into a human-readable string.
pub fn summarize_checks(checks: &[HashMap<String, Value>]) -> String {
    if checks.is_empty() {
        return "no checks".to_string();
    }
    let mut parts: Vec<String> = Vec::new();
    for check in checks {
        let name = check.get("name").map(as_string).unwrap_or_default();
        let conclusion = check.get("conclusion").map(as_string).unwrap_or_default();
        if !name.is_empty() {
            parts.push(format!("{}: {}", name, conclusion));
        }
    }
    if parts.is_empty() {
        format!("{} checks", checks.len())
    } else {
        parts.join(", ")
    }
}

/// Count unresolved review threads from raw JSON.
pub fn count_unresolved_threads(comments: &[HashMap<String, Value>]) -> i32 {
    let mut count = 0;
    for comment in comments {
        if let Some(is_resolved) = comment.get("isResolved") {
            if !as_bool(is_resolved) {
                count += 1;
            }
        }
    }
    count
}

/// Parse a PR number from a GitHub PR URL.
pub fn parse_pr_number_from_url(url: &str) -> i64 {
    // URL format: https://github.com/owner/repo/pull/123
    if let Some(pull_part) = url.split("/pull/").nth(1) {
        if let Some(num) = pull_part.split('/').next() {
            return num.parse().unwrap_or(0);
        }
    }
    0
}

/// Normalize a GitHub login (lowercase).
pub fn normalize_github_login(login: &str) -> String {
    login.to_lowercase()
}

// ---------------------------------------------------------------------------
// Label helpers
// ---------------------------------------------------------------------------

/// Resolve a hex color for a given label name.
pub fn resolve_label_color(label: &str) -> &str {
    match label {
        "looper:plan" => "C5DEF5",
        "looper:spec-reviewing" => "FBCA04",
        "looper:spec-ready" => "0E8A16",
        "looper:needs-human" => "B60205",
        "bug" => "D73A4A",
        "enhancement" => "A2EEEF",
        "documentation" => "0075CA",
        "question" => "D876E3",
        "good first issue" => "7057FF",
        "wontfix" => "FFFFFF",
        _ => "C0C0C0",
    }
}

/// Resolve a description for a given label name.
pub fn resolve_label_description(label: &str) -> &str {
    match label {
        "looper:plan" => "Looper has created a plan for this issue",
        "looper:spec-reviewing" => "Looper is reviewing the specification",
        "looper:spec-ready" => "Specification is ready for implementation",
        "looper:needs-human" => "This issue needs human intervention",
        _ => "",
    }
}

/// Normalize a label color to a 6-character hex string (with leading # stripped).
pub fn normalize_label_color(value: &str) -> String {
    let trimmed = value.trim_start_matches('#');
    if trimmed.len() == 3 {
        // Expand 3-digit hex to 6-digit
        trimmed.chars().flat_map(|c| std::iter::repeat_n(c, 2)).collect()
    } else {
        trimmed.to_uppercase()
    }
}

/// Increment a label init summary based on status.
pub fn increment_label_summary(summary: &mut LabelInitSummary, status: &str) {
    match status {
        "created" => summary.created += 1,
        "updated" => summary.updated += 1,
        "skipped" => summary.skipped += 1,
        "failed" => summary.failed += 1,
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Review submit helpers
// ---------------------------------------------------------------------------

/// Build the JSON request body for a review submission.
pub fn review_submit_request(input: &crate::types::SubmitReviewInput) -> HashMap<String, Value> {
    let mut map = HashMap::new();
    map.insert("event".into(), Value::String(input.event.clone()));
    map.insert("body".into(), if input.body.is_empty() { Value::Null } else { Value::String(input.body.clone()) });
    map.insert(
        "commit_id".into(),
        if input.commit_id.is_empty() { Value::Null } else { Value::String(input.commit_id.clone()) },
    );
    if !input.comments.is_empty() {
        let comments: Vec<Value> = input
            .comments
            .iter()
            .map(|c| {
                let mut cm = serde_json::Map::new();
                cm.insert("body".into(), Value::String(c.body.clone()));
                cm.insert("path".into(), Value::String(c.path.clone()));
                cm.insert("line".into(), Value::Number(c.line.into()));
                cm.insert("side".into(), Value::String(c.side.clone()));
                if c.start_line > 0 {
                    cm.insert("start_line".into(), Value::Number(c.start_line.into()));
                }
                if !c.start_side.is_empty() {
                    cm.insert("start_side".into(), Value::String(c.start_side.clone()));
                }
                Value::Object(cm)
            })
            .collect();
        map.insert("comments".into(), Value::Array(comments));
    }
    map
}

/// Build a summary map of the review body for diagnostics.
pub fn review_submit_body_marker_summary(body: &str) -> HashMap<String, Value> {
    let mut map = HashMap::new();
    map.insert("length".into(), Value::Number(body.len().into()));
    map.insert("has_marker".into(), Value::Bool(body.contains("looper-review:")));
    map
}

/// Build a summary of the review comments for diagnostics.
pub fn review_submit_comments_summary(comments: &[ReviewComment]) -> Vec<HashMap<String, Value>> {
    comments
        .iter()
        .map(|c| {
            let mut map = HashMap::new();
            map.insert("path".into(), Value::String(c.path.clone()));
            map.insert("line".into(), Value::Number(c.line.into()));
            map.insert("side".into(), Value::String(c.side.clone()));
            map
        })
        .collect()
}

/// Normalize inline review disclosure text (stamp/strip per config).
pub fn normalize_inline_review_disclosure(
    body: &str,
    _disclosure_cfg: &looper_config::types::DisclosureConfig,
) -> String {
    if !has_inline_review_disclosure(body) {
        return body.to_string();
    }
    strip_inline_review_disclosure(body)
}

/// Check if a review body has an inline disclosure stamp.
pub fn has_inline_review_disclosure(body: &str) -> bool {
    contains_visible_inline_review_disclosure(body) || body.contains("<!-- looper:disclosure")
}

/// Check if there's a visible (non-HTML-comment) disclosure.
pub fn contains_visible_inline_review_disclosure(body: &str) -> bool {
    body.contains("_This review was generated by_")
        || body.contains("_Powered by Looper_")
        || body.contains("_Looper AI_")
}

/// Strip inline review disclosure from a body.
pub fn strip_inline_review_disclosure(body: &str) -> String {
    let mut result = String::new();
    let mut in_disclosure = false;
    for line in body.lines() {
        if line.trim().starts_with("<!-- looper:disclosure") {
            in_disclosure = true;
            continue;
        }
        if in_disclosure && line.trim() == "-->" {
            in_disclosure = false;
            continue;
        }
        if in_disclosure {
            continue;
        }
        // Also strip visible disclosure sections
        if line.contains("_This review was generated by_")
            || line.contains("_Powered by Looper_")
            || line.contains("_Looper AI_")
        {
            continue;
        }
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(line);
    }
    result.trim().to_string()
}

// ---------------------------------------------------------------------------
// Review thread GraphQL helpers
// ---------------------------------------------------------------------------

/// Normalize a raw JSON review thread node into a structured map.
pub fn normalize_review_thread(value: &Value) -> Option<HashMap<String, Value>> {
    let obj = value.as_object()?;
    let mut result = HashMap::new();
    result.insert("id".into(), Value::String(obj.get("id").map(as_string).unwrap_or_default()));
    result.insert("isResolved".into(), Value::Bool(as_bool(obj.get("isResolved").unwrap_or(&Value::Bool(false)))));
    result.insert("path".into(), Value::String(obj.get("path").map(as_string).unwrap_or_default()));
    result.insert("line".into(), Value::Number(obj.get("line").and_then(|v| v.as_i64()).unwrap_or(0).into()));
    // Extract comments from the nested comments connection
    if let Some(comments_conn) = obj.get("comments") {
        if let Some(comments_obj) = comments_conn.as_object() {
            if let Some(nodes) = comments_obj.get("nodes") {
                result.insert("comments".into(), nodes.clone());
            }
        }
    }
    Some(result)
}

/// Produce a fingerprint of review thread nodes for deduplication.
pub fn review_thread_fingerprint_from_nodes(nodes: &[Value]) -> String {
    let mut ids: Vec<String> =
        nodes.iter().filter_map(|v| v.as_object()).filter_map(|m| m.get("id")).map(as_string).collect();
    ids.sort();
    ids.join(",")
}

/// Append review thread comments from GraphQL nodes.
pub fn append_review_thread_comment(dst: &mut Vec<ReviewThreadComment>, nodes: &[Value]) {
    for node in nodes {
        if let Some(obj) = node.as_object() {
            dst.push(ReviewThreadComment {
                id: obj.get("id").map(as_string).unwrap_or_default(),
                body: obj.get("body").map(as_string).unwrap_or_default(),
                author: obj
                    .get("author")
                    .and_then(|a| a.as_object())
                    .and_then(|a| a.get("login"))
                    .map(as_string)
                    .unwrap_or_default(),
                author_association: obj.get("authorAssociation").map(as_string).unwrap_or_default(),
                created_at: obj.get("createdAt").map(as_string).unwrap_or_default(),
                updated_at: obj.get("updatedAt").map(as_string).unwrap_or_default(),
                path: obj.get("path").map(as_string).unwrap_or_default(),
                line: obj.get("line").map(as_i64).unwrap_or(0),
                original_commit_oid: obj
                    .get("originalCommit")
                    .and_then(|c| c.as_object())
                    .and_then(|c| c.get("oid"))
                    .map(as_string)
                    .unwrap_or_default(),
                commit_oid: obj
                    .get("commit")
                    .and_then(|c| c.as_object())
                    .and_then(|c| c.get("oid"))
                    .map(as_string)
                    .unwrap_or_default(),
                url: obj.get("url").map(as_string).unwrap_or_default(),
            });
        }
    }
}

/// Extract a review thread node (id + resolved) from GraphQL response.
pub fn get_review_thread_node(value: &Value) -> Option<crate::types::ReviewThreadNode> {
    let obj = value.as_object()?;
    let id = obj.get("id").map(as_string)?;
    let is_resolved = obj.get("isResolved").map(as_bool).unwrap_or(false);
    Some(crate::types::ReviewThreadNode { id, is_resolved })
}
