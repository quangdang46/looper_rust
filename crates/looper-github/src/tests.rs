//! Tests for the looper-github crate.
//!
//! Gateway methods are tested via the `gh_run` injection mechanism:
//! `GatewayOptions.gh_run` provides a fake shell runner that returns
//! predetermined JSON responses without invoking the real `gh` CLI.

use std::sync::Arc;
use std::time::Duration;

use crate::cache::DiscoveryCache;
use crate::error::*;
use crate::gateway::ShellResult;
use crate::gateway::{Gateway, GatewayOptions};
use crate::helpers::*;
use crate::types::*;

// ---------------------------------------------------------------------------
// Error classification tests
// ---------------------------------------------------------------------------

#[test]
fn test_is_transient_error_transient_variant() {
    let err = GitHubError::Transient(TransientError::new("network blip"));
    assert!(is_transient_error(&err));
}

#[test]
fn test_is_transient_error_command_execution_with_transient_msg() {
    let err = GitHubError::CommandExecution("tls handshake timeout".into());
    assert!(is_transient_error(&err));
}

#[test]
fn test_is_transient_error_non_transient_msg() {
    let err = GitHubError::CommandExecution("permission denied".into());
    assert!(!is_transient_error(&err));
}

#[test]
fn test_is_transient_error_rate_limit() {
    let err = GitHubError::RateLimit("API rate limit exceeded".into());
    assert!(is_transient_error(&err));
}

#[test]
fn test_is_transient_error_other_variants() {
    assert!(!is_transient_error(&GitHubError::NotFound("nf".into())));
    assert!(!is_transient_error(&GitHubError::Auth("bad".into())));
    assert!(!is_transient_error(&GitHubError::Empty("e".into())));
}

#[test]
fn test_error_message_formats() {
    for (err, expected) in [
        (GitHubError::CommandExecution("cmd failed".into()), "cmd failed"),
        (GitHubError::NotFound("missing".into()), "missing"),
        (GitHubError::RateLimit("slow".into()), "slow"),
        (GitHubError::Auth("bad token".into()), "bad token"),
    ] {
        assert_eq!(error_message(&err), expected);
    }
}

#[test]
fn test_error_message_api() {
    assert_eq!(error_message(&GitHubError::Api("nf".into(), 404)), "nf (HTTP 404)");
}

#[test]
fn test_error_message_review_thread_not_found() {
    assert_eq!(error_message(&GitHubError::ReviewThreadNotFound { thread_id: "thr_1".into() }), "thr_1");
}

#[test]
fn test_is_pull_request_not_found_error() {
    assert!(is_pull_request_not_found_error(&GitHubError::CommandFailed(
        "GraphQL: could not resolve to a pullrequest".into()
    )));
    assert!(is_pull_request_not_found_error(&GitHubError::NotFound("pull request not found".into())));
    assert!(!is_pull_request_not_found_error(&GitHubError::Auth("bad".into())));
}

#[test]
fn test_is_not_found_error() {
    assert!(is_not_found_error(&GitHubError::NotFound("gone".into())));
    assert!(is_not_found_error(&GitHubError::Api("nf".into(), 404)));
    assert!(is_not_found_error(&GitHubError::CommandFailed("HTTP 404".into())));
    assert!(!is_not_found_error(&GitHubError::Auth("bad".into())));
}

#[test]
fn test_is_inaccessible_review_request_reviewer_error() {
    assert!(is_inaccessible_review_request_reviewer_error(&GitHubError::CommandFailed(
        "Resource not accessible by integration: reviewRequests/requestedReviewer".into()
    )));
    assert!(!is_inaccessible_review_request_reviewer_error(&GitHubError::CommandFailed("other".into())));
}

#[test]
fn test_is_diff_too_large_error() {
    assert!(is_diff_too_large_error(&GitHubError::DiffTooLarge("big diff".into())));
    assert!(!is_diff_too_large_error(&GitHubError::NotFound("nope".into())));
}

// ---------------------------------------------------------------------------
// DiscoveryCache tests
// ---------------------------------------------------------------------------

fn dummy_pr(number: i64) -> PullRequestSummary {
    PullRequestSummary {
        number,
        title: "".into(),
        url: "".into(),
        state: "OPEN".into(),
        updated_at: "".into(),
        is_draft: false,
        review_decision: "".into(),
        labels: vec![],
        head_ref_name: "".into(),
        base_ref_name: "".into(),
        head_sha: "".into(),
        base_sha: "".into(),
        has_conflicts: false,
        author: "".into(),
        author_association: "".into(),
        review_requests: vec![],
        review_request_users: vec![],
        reviews: vec![],
    }
}

fn dummy_issue(number: i64) -> IssueSummary {
    IssueSummary {
        number,
        title: "".into(),
        body: "".into(),
        url: "".into(),
        state: "OPEN".into(),
        updated_at: "".into(),
        author: "".into(),
        author_association: "".into(),
        assignees: vec![],
        assignee_users: vec![],
        labels: vec![],
        is_pull_request: false,
    }
}

#[test]
fn test_cache_new_misses() {
    let c = DiscoveryCache::new();
    assert!(c.get_prs("k", Duration::from_secs(30)).is_none());
    assert!(c.get_review_prs("k", Duration::from_secs(30)).is_none());
    assert!(c.get_issues("k", Duration::from_secs(30)).is_none());
}

#[test]
fn test_cache_prs_set_get() {
    let c = DiscoveryCache::new();
    c.set_prs("k".into(), vec![dummy_pr(1)], Duration::from_secs(60));
    assert!(c.get_prs("k", Duration::from_secs(60)).is_some());
}

#[test]
fn test_cache_review_prs_set_get() {
    let c = DiscoveryCache::new();
    c.set_review_prs("k".into(), vec![dummy_pr(2)], Duration::from_secs(10));
    assert!(c.get_review_prs("k", Duration::from_secs(10)).is_some());
    assert!(c.get_review_prs("other", Duration::from_secs(10)).is_none());
}

#[test]
fn test_cache_issues_set_get() {
    let c = DiscoveryCache::new();
    c.set_issues("k".into(), vec![dummy_issue(42)], Duration::from_secs(30));
    assert!(c.get_issues("k", Duration::from_secs(30)).is_some());
}

#[test]
fn test_cache_default() {
    let c = DiscoveryCache::default();
    assert!(c.get_prs("k", Duration::from_secs(5)).is_none());
}

// ---------------------------------------------------------------------------
// Helper function tests
// ---------------------------------------------------------------------------

#[test]
fn test_decode_json_object() {
    let r = decode_json_object(r#"{"a":1}"#).unwrap();
    assert_eq!(r.get("a").and_then(|v| v.as_i64()), Some(1));
    assert!(decode_json_object("bad").is_err());
}

#[test]
fn test_decode_json_array() {
    let r = decode_json_array(r#"[{"n":1}]"#).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(decode_json_array("[]").unwrap().len(), 0);
}

#[test]
fn test_decode_json_array_or_pages_single() {
    let r = decode_json_array_or_pages(r#"[{"id":1}]"#).unwrap();
    assert_eq!(r.len(), 1);
}

#[test]
fn test_decode_json_array_or_pages_nested() {
    let r = decode_json_array_or_pages(r#"[[{"id":1}],[{"id":2}]]"#).unwrap();
    assert_eq!(r.len(), 2);
}

#[test]
fn test_decode_json_array_or_pages_invalid() {
    assert!(decode_json_array_or_pages("null").unwrap().is_empty());
}

#[test]
fn test_as_string() {
    assert_eq!(as_string(&serde_json::json!("hi")), "hi");
    assert_eq!(as_string(&serde_json::json!(42)), "42");
    assert_eq!(as_string(&serde_json::Value::Bool(true)), "true");
    assert_eq!(as_string(&serde_json::Value::Null), "");
}

#[test]
fn test_as_bool() {
    assert!(as_bool(&serde_json::Value::Bool(true)));
    assert!(!as_bool(&serde_json::Value::Bool(false)));
    assert!(!as_bool(&serde_json::Value::Null));
}

#[test]
fn test_as_i64() {
    assert_eq!(as_i64(&serde_json::json!(42)), 42);
    assert_eq!(as_i64(&serde_json::Value::Null), 0);
}

#[test]
fn test_bool_ptr_from_value() {
    assert_eq!(bool_ptr_from_value(&serde_json::Value::Bool(true)), Some(true));
    assert_eq!(bool_ptr_from_value(&serde_json::Value::Null), None);
}

#[test]
fn test_to_object_slice() {
    assert_eq!(to_object_slice(&serde_json::json!([{"a":1}])).len(), 1);
    assert!(to_object_slice(&serde_json::json!("s")).is_empty());
}

#[test]
fn test_nested_string_access() {
    let mut m = std::collections::HashMap::new();
    m.insert("a".into(), serde_json::json!({"b": "c"}));
    assert_eq!(nested_string(&m, &["a", "b"]), "c");
    assert_eq!(nested_string(&m, &["a", "x"]), "");
    assert_eq!(nested_string(&m, &["x"]), "");
}

#[test]
fn test_first_non_empty_returns_first() {
    assert_eq!(first_non_empty(&["", "", "x"]), "x");
    assert_eq!(first_non_empty(&["", ""]), "");
}

#[test]
fn test_extract_author() {
    assert_eq!(extract_author(&serde_json::json!({"login": "u"})), "u");
    assert_eq!(extract_author(&serde_json::Value::Null), "");
}

#[test]
fn test_extract_label_names() {
    assert_eq!(extract_label_names(&serde_json::json!([{"name":"bug"}])), vec!["bug"]);
    assert!(extract_label_names(&serde_json::Value::Null).is_empty());
}

#[test]
fn test_extract_auto_merge() {
    let v = serde_json::json!({"enabledBy":{"login":"b"},"mergeMethod":"SQ"});
    let am = extract_auto_merge(&v).unwrap();
    assert_eq!(am.enabled_by, "b");
    assert_eq!(am.merge_method, "SQ");
    assert!(extract_auto_merge(&serde_json::Value::Null).is_none());
}

#[test]
fn test_parse_repo_variants() {
    // parse_repo splits on "/" and expects at least 2 non-empty parts
    let r = parse_repo("owner/repo.git").unwrap();
    assert_eq!(r, ("owner".into(), "repo.git".into()));
    let r = parse_repo("owner/repo").unwrap();
    assert_eq!(r, ("owner".into(), "repo".into()));
    assert!(parse_repo("").is_err());
}

#[test]
fn test_split_repo_owner_name_works() {
    let (o, n) = split_repo_owner_name("owner/repo");
    assert_eq!(o, "owner");
    assert_eq!(n, "repo");
}

#[test]
fn test_validate_github_repo_slug() {
    assert!(validate_github_repo_slug("owner/repo").is_ok());
    assert!(validate_github_repo_slug("").is_err());
    assert!(validate_github_repo_slug("not-a-slug").is_err());
}

#[test]
fn test_review_event_from_state_mapping() {
    assert_eq!(review_event_from_state("APPROVED"), "APPROVE");
    assert_eq!(review_event_from_state("CHANGES_REQUESTED"), "REQUEST_CHANGES");
    assert_eq!(review_event_from_state("COMMENTED"), "COMMENT");
    assert_eq!(review_event_from_state("PENDING"), "PENDING");
    assert_eq!(review_event_from_state("unknown"), "unknown");
}

#[test]
fn test_default_limit_clamps() {
    assert_eq!(default_limit(0), 30);
    assert_eq!(default_limit(10), 30); // clamped to minimum 30
    assert_eq!(default_limit(100), 100);
}

#[test]
fn test_random_id_non_empty() {
    assert!(!random_id().is_empty());
}

#[test]
fn test_encode_uri_component_encodes() {
    assert_eq!(encode_uri_component("a b/c"), "a%20b%2Fc");
}

#[test]
fn test_unique_strings_dedup() {
    let r = unique_strings(&["a".into(), "b".into(), "a".into()]);
    assert_eq!(r.len(), 2);
}

#[test]
fn test_parse_pr_number_from_url() {
    assert_eq!(parse_pr_number_from_url("https://github.com/o/r/pull/42"), 42);
    assert_eq!(parse_pr_number_from_url(""), 0);
}

#[test]
fn test_normalize_github_login() {
    assert_eq!(normalize_github_login("[BOT] user"), "[bot] user");
    assert_eq!(normalize_github_login("  spaces  "), "  spaces  ");
    assert_eq!(normalize_github_login("normal"), "normal");
}

#[test]
fn test_resolve_label_color() {
    assert_eq!(resolve_label_color("looper:plan"), "C5DEF5");
    assert_eq!(resolve_label_color("unknown"), "C0C0C0");
}

#[test]
fn test_normalize_label_color() {
    assert_eq!(normalize_label_color("#C5DEF5"), "C5DEF5");
    assert_eq!(normalize_label_color("C5DEF5"), "C5DEF5");
}

#[test]
fn test_has_inline_review_disclosure() {
    assert!(has_inline_review_disclosure("_This review was generated by_Looper_"));
    assert!(!has_inline_review_disclosure("no"));
}

#[test]
fn test_strip_inline_review_disclosure() {
    let s = strip_inline_review_disclosure("a\n<looper:review-summary>\ns\n</looper:review-summary>\nb");
    assert!(!s.contains("secret"));
    assert!(s.contains("a"));
    assert!(s.contains("b"));
}

// ---------------------------------------------------------------------------
// Types tests
// ---------------------------------------------------------------------------

#[test]
fn test_standard_looper_labels() {
    let labels = standard_looper_labels();
    assert_eq!(labels.len(), 4);
    assert!(labels.iter().any(|l| l.name == "looper:plan"));
    assert!(labels.iter().any(|l| l.name == "looper:needs-human"));
    assert!(labels.iter().all(|l| l.color.len() == 6));
}

#[test]
fn test_label_definition_serde() {
    let l = LabelDefinition { name: "l".into(), color: "FFF".into(), description: "d".into() };
    let j = serde_json::to_string(&l).unwrap();
    let back: LabelDefinition = serde_json::from_str(&j).unwrap();
    assert_eq!(back.name, "l");
}

#[test]
fn test_pull_request_summary_deserialize() {
    let pr: PullRequestSummary = serde_json::from_value(serde_json::json!({
        "number": 1, "title": "t", "url": "u", "state": "OPEN",
        "updated_at": "2024-01-01T00:00:00Z", "is_draft": false,
        "review_decision": "A", "labels": [], "head_ref_name": "h",
        "base_ref_name": "b", "head_sha": "a", "base_sha": "d",
        "has_conflicts": false, "author": "u", "author_association": "M",
        "review_requests": [], "review_request_users": [], "reviews": [],
    }))
    .unwrap();
    assert_eq!(pr.number, 1);
    assert_eq!(pr.title, "t");
}

#[test]
fn test_pull_request_detail_deserialize() {
    let pr: PullRequestDetail = serde_json::from_value(serde_json::json!({
        "number": 1, "title": "t", "body": "b", "url": "u", "state": "OPEN",
        "created_at": "", "updated_at": "", "closed_at": "", "is_draft": false,
        "review_decision": "A", "labels": [], "head_ref_name": "h", "base_ref_name": "b",
        "head_sha": "a", "base_sha": "d", "author": "u", "author_association": "M",
        "comment_count": 3, "review_requests": [], "review_request_users": [],
        "has_conflicts": false, "comments": [], "issue_comments": [], "reviews": [],
        "checks": [], "mergeable_state": "", "merged_at": "",
    }))
    .unwrap();
    assert_eq!(pr.number, 1);
    assert_eq!(pr.comment_count, 3);
}

// ---------------------------------------------------------------------------
// Gateway tests (via gh_run injection)
// ---------------------------------------------------------------------------

fn mock_gw(stdout: &str) -> Gateway {
    let s = stdout.to_string();
    Gateway::new(GatewayOptions {
        gh_run: Some(Arc::new(move |_| Ok(ShellResult { stdout: s.clone(), stderr: String::new(), exit_code: 0 }))),
        ..Default::default()
    })
}

fn mock_gw_err(msg: &str) -> Gateway {
    let msg: String = msg.into();
    Gateway::new(GatewayOptions {
        gh_run: Some(Arc::new(move |_| Err(GitHubError::CommandFailed(msg.clone())))),
        ..Default::default()
    })
}

#[test]
fn test_gw_new_default() {
    let g = Gateway::new(GatewayOptions::default());
    assert_eq!(g.gh_path, "gh");
    assert_eq!(g.cwd, ".");
}

#[test]
fn test_gw_detect_current_repository() {
    let g = mock_gw(r#"{"nameWithOwner":"owner/repo"}"#);
    assert_eq!(g.detect_current_repository(".").unwrap(), "owner/repo");
}

#[test]
fn test_gw_detect_current_repository_error() {
    let g = mock_gw_err("fail");
    assert!(g.detect_current_repository(".").is_err());
}

#[test]
fn test_gw_list_open_pull_requests() {
    let json = serde_json::json!([{
        "number": 1, "title": "PR1", "url": "u", "state": "OPEN",
        "updatedAt": "", "isDraft": false, "reviewDecision": "",
        "labels": [], "headRefName": "", "baseRefName": "",
        "headRefOid": "", "baseRefOid": "", "hasConflicts": false,
        "author": "", "authorAssociation": "",
        "reviewRequests": [], "reviews": []
    }]);
    let g = mock_gw(&json.to_string());
    let r = g.list_open_pull_requests(ListOpenPullRequestsInput { repo: "o/r".into(), ..Default::default() }).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].number, 1);
}

#[test]
fn test_gw_list_open_issues() {
    let json = serde_json::json!([{
        "number": 42, "title": "Bug", "body": "", "url": "u", "state": "OPEN",
        "updatedAt": "", "author": "u", "authorAssociation": "", "assignees": [], "labels": []
    }]);
    let g = mock_gw(&json.to_string());
    let r = g.list_open_issues(ListOpenIssuesInput { repo: "o/r".into(), ..Default::default() }).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].number, 42);
}

#[test]
fn test_gw_get_issue_state() {
    let g = mock_gw(r#"{"state":"OPEN","stateReason":"completed"}"#);
    let s = g.get_issue_state(ViewIssueInput { repo: "o/r".into(), issue_number: 1, ..Default::default() }).unwrap();
    assert_eq!(s.state, "OPEN");
    assert_eq!(s.state_reason, "completed");
}

#[test]
fn test_gw_get_current_user_identity() {
    let g = mock_gw(r#"{"login":"testuser","id":12345}"#);
    let u = g.get_current_user_identity(".").unwrap();
    assert_eq!(u.login, "testuser");
    assert_eq!(u.numeric_id, 12345);
}

#[test]
fn test_gw_get_current_user_login() {
    let g = mock_gw(r#"testuser"#);
    assert_eq!(g.get_current_user_login(".").unwrap(), "testuser");
}

#[test]
fn test_gw_get_repository_permission() {
    let g = mock_gw(r#"{"permission":"write"}"#);
    let p = g
        .get_repository_permission(RepositoryPermissionInput {
            repo: "o/r".into(),
            user: "u".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(p, "write");
}

#[test]
fn test_gw_get_repository_permission_read() {
    let g = mock_gw(r#"{"permission":"read"}"#);
    let p = g
        .get_repository_permission(RepositoryPermissionInput {
            repo: "o/r".into(),
            user: "v".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(p, "read");
}

#[test]
fn test_gw_view_pull_request() {
    let json = serde_json::json!({
        "number": 5, "title": "TPR", "body": "", "url": "", "state": "OPEN",
        "createdAt": "", "updatedAt": "", "closedAt": "", "isDraft": false,
        "reviewDecision": "", "labels": [], "headRefName": "", "baseRefName": "",
        "headRefOid": "", "baseRefOid": "", "author": "", "authorAssociation": "",
        "commentCount": 0, "reviewRequests": [], "reviewRequestUsers": [],
        "hasConflicts": false, "comments": [], "issueComments": [], "reviews": [],
        "checks": [], "mergeableState": "", "mergedAt": "",
    });
    let g = mock_gw(&json.to_string());
    let pr =
        g.view_pull_request(ViewPullRequestInput { repo: "o/r".into(), pr_number: 5, ..Default::default() }).unwrap();
    assert_eq!(pr.number, 5);
    assert_eq!(pr.title, "TPR");
}

#[test]
fn test_gw_list_empty_issues() {
    let g = mock_gw("[]");
    let r = g.list_open_issues(ListOpenIssuesInput { repo: "o/r".into(), ..Default::default() }).unwrap();
    assert!(r.is_empty());
}

#[test]
fn test_gw_list_pull_request_review_state() {
    // gh pr view --json reviewRequests,reviews outputs requestedReviewer sub-object
    let json = serde_json::json!({
        "reviewRequests": [{"requestedReviewer": {"login": "r1"}}],
        "reviews": [{"author":{"login":"u1"},"state":"APPROVED","submittedAt":"2024-01-01T00:00:00Z"}]
    });
    let g = mock_gw(&json.to_string());
    let s = g
        .list_pull_request_review_state(PullRequestReviewStateInput {
            repo: "o/r".into(),
            pr_number: 1,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(s.requested_reviewers, vec!["r1"]);
}

#[test]
fn test_gw_get_pull_request_diff() {
    let g = mock_gw("diff --git a/f.rs b/f.rs\n-old\n+new\n");
    let d = g
        .get_pull_request_diff(GetPullRequestDiffInput { repo: "o/r".into(), pr_number: 1, ..Default::default() })
        .unwrap();
    assert!(d.contains("+new"));
}

#[test]
fn test_gw_list_linked_pull_requests() {
    // GraphQL response format: {"data":{"repository":{"issue":{"closedByPullRequestsReferences":{"nodes":[...]}}}}}
    let json = serde_json::json!({
        "data": {
            "repository": {
                "issue": {
                    "closedByPullRequestsReferences": {
                        "nodes": [{"number": 2, "state": "OPEN", "merged": false,
                            "mergedAt": "", "mergeCommit":{"oid":""}}]
                    }
                }
            }
        }
    });
    let g = mock_gw(&json.to_string());
    let r = g
        .list_linked_pull_requests(LinkedPullRequestsInput {
            repo: "o/r".into(),
            issue_number: 1,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].number, 2);
}

#[test]
fn test_gw_get_pull_request_head_sha() {
    let g = mock_gw("abc123");
    let sha = g
        .get_pull_request_head_sha(ViewPullRequestInput { repo: "o/r".into(), pr_number: 1, ..Default::default() })
        .unwrap();
    assert_eq!(sha, "abc123");
}

#[test]
fn test_gw_list_pull_request_check_runs() {
    // gh API returns snake_case fields
    let json = serde_json::json!({
        "total_count": 1,
        "check_runs": [{"name":"CI","status":"COMPLETED","conclusion":"SUCCESS"}],
        "statuses": []
    });
    let g = mock_gw(&json.to_string());
    let r = g
        .list_pull_request_check_runs(PullRequestCheckRunsInput {
            repo: "o/r".into(),
            r#ref: "main".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(r.total_count, 1);
}

#[test]
fn test_gw_create_pull_request() {
    // gh pr create outputs a URL like https://github.com/owner/repo/pull/42
    let g = mock_gw("https://github.com/o/r/pull/42");
    let pr = g
        .create_pull_request(CreatePullRequestInput {
            repo: "o/r".into(),
            title: "New".into(),
            head_branch: "f".into(),
            base_branch: "main".into(),
            body: "X".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(pr.number, 42);
}

#[test]
fn test_gw_enable_auto_merge() {
    let g = mock_gw("");
    let r = g.enable_auto_merge(EnableAutoMergeInput {
        repo: "o/r".into(),
        pr_number: 1,
        head_sha: "abc".into(),
        strategy: "SQUASH".into(),
        ..Default::default()
    });
    assert!(r.is_ok());
}

#[test]
fn test_gw_update_pull_request_body() {
    let g = mock_gw(r#"{"number":1}"#);
    assert!(g
        .update_pull_request_body(UpdatePullRequestBodyInput {
            repo: "o/r".into(),
            pr_number: 1,
            body: "U".into(),
            ..Default::default()
        })
        .is_ok());
}

#[test]
fn test_gw_update_pull_request_title() {
    let g = mock_gw(r#"{"number":1}"#);
    assert!(g
        .update_pull_request_title(UpdatePullRequestTitleInput {
            repo: "o/r".into(),
            pr_number: 1,
            title: "N".into(),
            ..Default::default()
        })
        .is_ok());
}

#[test]
fn test_gw_close_pull_request() {
    let g = mock_gw("");
    assert!(g
        .close_pull_request(ClosePullRequestInput { repo: "o/r".into(), pr_number: 1, ..Default::default() })
        .is_ok());
}

#[test]
fn test_gw_close_issue() {
    let g = mock_gw("");
    assert!(g.close_issue(CloseIssueInput { repo: "o/r".into(), issue_number: 1, ..Default::default() }).is_ok());
}

#[test]
fn test_gw_add_issue_labels() {
    let g = mock_gw("[]");
    assert!(g
        .add_issue_labels(IssueLabelsInput {
            repo: "o/r".into(),
            issue_number: 1,
            labels: vec!["bug".into()],
            ..Default::default()
        })
        .is_ok());
}

#[test]
fn test_gw_remove_issue_labels() {
    let g = mock_gw("");
    assert!(g
        .remove_issue_labels(IssueLabelsInput {
            repo: "o/r".into(),
            issue_number: 1,
            labels: vec!["wontfix".into()],
            ..Default::default()
        })
        .is_ok());
}

#[test]
fn test_gw_add_issue_assignees() {
    let g = mock_gw("");
    assert!(g
        .add_issue_assignees(IssueAssigneesInput {
            repo: "o/r".into(),
            issue_number: 1,
            assignees: vec!["u1".into()],
            ..Default::default()
        })
        .is_ok());
}

#[test]
fn test_gw_get_pull_request_head_and_author() {
    // gh pr view --json headRefOid,author outputs author as {"login":"dev",...}
    let g = mock_gw(r#"{"headRefOid":"abc123","author":{"login":"dev"}}"#);
    let ha = g
        .get_pull_request_head_and_author(ViewPullRequestInput {
            repo: "o/r".into(),
            pr_number: 1,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(ha.head_sha, "abc123");
    assert_eq!(ha.author, "dev");
}

#[test]
fn test_gw_add_pull_request_reviewers() {
    let g = mock_gw("");
    assert!(g
        .add_pull_request_reviewers(PullRequestReviewersInput {
            repo: "o/r".into(),
            pr_number: 1,
            reviewers: vec!["r1".into()],
            ..Default::default()
        })
        .is_ok());
}

#[test]
fn test_gw_add_pull_request_comment() {
    let g = mock_gw(r#"{"id":200,"url":"u"}"#);
    let c = g.add_pull_request_comment(PullRequestCommentInput {
        repo: "o/r".into(),
        pr_number: 1,
        body: "LGTM".into(),
        ..Default::default()
    });
    assert!(c.is_ok());
}

#[test]
fn test_gw_submit_review() {
    let g = mock_gw(r#"{"id":"r1","state":"APPROVED","body":"ok"}"#);
    assert!(g
        .submit_review(SubmitReviewInput {
            repo: "o/r".into(),
            pr_number: 1,
            commit_id: "a".into(),
            event: "APPROVE".into(),
            body: "LGTM".into(),
            cwd: ".".into(),
            comments: vec![],
            anchors: None,
            disclosure: Default::default(),
        })
        .is_ok());
}

#[test]
fn test_gw_compare_branches() {
    let g = mock_gw(r#"{"status":"diverged","aheadBy":3,"behindBy":1}"#);
    let c = g
        .compare_branches(CompareBranchesInput {
            repo: "o/r".into(),
            base_branch: "main".into(),
            head_branch: "f".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(c.status, "diverged");
}

#[test]
fn test_gw_is_authenticated() {
    let g = mock_gw("true");
    assert!(g.is_authenticated(".", "github.com").unwrap());
}

#[test]
fn test_gw_find_any_issue_number() {
    let g = mock_gw(r#"[{"number":42}]"#);
    assert_eq!(g.find_any_issue_number("o/r", ".").unwrap(), 42);
}

#[test]
fn test_gw_list_issue_comments() {
    let json = serde_json::json!([{
        "id": 100, "author": {"login": "u"}, "authorAssociation": "NONE",
        "body": "thanks", "createdAt": "", "updatedAt": "",
        "url": "https://github.com/o/r/issues/1#issuecomment-100"
    }]);
    let g = mock_gw(&json.to_string());
    let r =
        g.list_issue_comments(ViewIssueInput { repo: "o/r".into(), issue_number: 1, ..Default::default() }).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].id, 100);
}
