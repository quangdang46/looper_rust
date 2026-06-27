//! Input/output types for the GitHub gateway.

use std::collections::HashMap;

use looper_config::types::DisclosureConfig;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// PR / Issue summary types
// ---------------------------------------------------------------------------

/// Summary of a pull request (from list operations).
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PullRequestSummary {
    pub number: i64,
    pub title: String,
    pub url: String,
    pub state: String,
    pub updated_at: String,
    pub is_draft: bool,
    pub review_decision: String,
    pub labels: Vec<String>,
    pub head_ref_name: String,
    pub base_ref_name: String,
    pub head_sha: String,
    pub base_sha: String,
    pub has_conflicts: bool,
    pub author: String,
    pub author_association: String,
    pub review_requests: Vec<String>,
    pub review_request_users: Vec<GitHubUser>,
    pub reviews: Vec<HashMap<String, Value>>,
}

/// Full detail of a pull request (from view operations).
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PullRequestDetail {
    pub number: i64,
    pub title: String,
    pub body: String,
    pub url: String,
    pub state: String,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: String,
    pub is_draft: bool,
    pub review_decision: String,
    pub labels: Vec<String>,
    pub head_ref_name: String,
    pub base_ref_name: String,
    pub head_sha: String,
    pub base_sha: String,
    pub author: String,
    pub author_association: String,
    pub comment_count: i32,
    pub review_requests: Vec<String>,
    pub review_request_users: Vec<GitHubUser>,
    pub has_conflicts: bool,
    pub comments: Vec<HashMap<String, Value>>,
    pub issue_comments: Vec<CommentInfo>,
    pub reviews: Vec<HashMap<String, Value>>,
    pub checks: Vec<HashMap<String, Value>>,
    pub mergeable: Option<bool>,
    pub mergeable_state: String,
    pub merged_at: String,
    pub auto_merge: Option<PullRequestAutoMerge>,
}

/// Auto-merge settings on a pull request.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PullRequestAutoMerge {
    pub enabled_by: String,
    pub merge_method: String,
}

// ---------------------------------------------------------------------------
// Check runs
// ---------------------------------------------------------------------------

/// Check runs and statuses for a commit.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PullRequestCheckRuns {
    pub total_count: i32,
    pub check_runs: Vec<PullRequestCheckRun>,
    pub statuses: Vec<PullRequestStatus>,
}

/// A single check run.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PullRequestCheckRun {
    pub name: String,
    pub status: String,
    pub conclusion: String,
}

/// A commit status check.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PullRequestStatus {
    pub context: String,
    pub state: String,
}

// ---------------------------------------------------------------------------
// Comments
// ---------------------------------------------------------------------------

/// Information about an issue/PR comment.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CommentInfo {
    pub id: i64,
    pub author: String,
    pub author_association: String,
    pub body: String,
    pub created_at: String,
    pub updated_at: String,
    pub url: String,
}

// ---------------------------------------------------------------------------
// User / identity
// ---------------------------------------------------------------------------

/// A GitHub user.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GitHubUser {
    pub login: String,
    pub id: i64,
}

/// Identity of the authenticated user.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CurrentUserIdentity {
    pub login: String,
    pub numeric_id: i64,
}

// ---------------------------------------------------------------------------
// Issues
// ---------------------------------------------------------------------------

/// Summary of an issue.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IssueSummary {
    pub number: i64,
    pub title: String,
    pub body: String,
    pub url: String,
    pub state: String,
    pub updated_at: String,
    pub author: String,
    pub author_association: String,
    pub assignees: Vec<String>,
    pub assignee_users: Vec<GitHubUser>,
    pub labels: Vec<String>,
    pub is_pull_request: bool,
}

/// Full detail of an issue.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IssueDetail {
    pub number: i64,
    pub title: String,
    pub body: String,
    pub url: String,
    pub state: String,
    pub state_reason: String,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: String,
    pub author: String,
    pub author_association: String,
    pub assignees: Vec<String>,
    pub assignee_users: Vec<GitHubUser>,
    pub labels: Vec<String>,
    pub is_pull_request: bool,
    pub comment_count: i32,
    pub comments: Vec<CommentInfo>,
}

/// Represents a repository referenced by an issue dependency.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IssueRepository {
    pub name: String,
    pub full_name: String,
    pub url: String,
    pub html_url: String,
}

/// A dependency issue (blocked-by / blocking / sub-issue).
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DependencyIssue {
    pub id: i64,
    pub number: i64,
    pub title: String,
    pub url: String,
    pub html_url: String,
    pub repository_url: String,
    pub state: String,
    pub state_reason: String,
    pub repository: IssueRepository,
}

/// Issue state and reason.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IssueState {
    pub state: String,
    pub state_reason: String,
}

// ---------------------------------------------------------------------------
// Input types for various operations
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IssueTimelineInput {
    pub repo: String,
    pub issue_number: i64,
    pub cwd: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IssueReactionInput {
    pub repo: String,
    pub issue_number: i64,
    pub comment_id: i64,
    pub cwd: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CreateIssueReactionInput {
    pub repo: String,
    pub issue_number: i64,
    pub comment_id: i64,
    pub content: String,
    pub cwd: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct RepositoryPermissionInput {
    pub repo: String,
    pub user: String,
    pub cwd: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ListIssueBlockedByInput {
    pub repo: String,
    pub issue_number: i64,
    pub cwd: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IssueDependency {
    pub number: i64,
    pub repo: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct LinkedPullRequestsInput {
    pub repo: String,
    pub issue_number: i64,
    pub cwd: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PullRequestReviewStateInput {
    pub repo: String,
    pub pr_number: i64,
    pub cwd: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IssueReaction {
    pub id: i64,
    pub content: String,
    pub user_login: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LinkedPullRequest {
    pub number: i64,
    pub state: String,
    pub merged: bool,
    pub merged_at: String,
    pub merge_commit_sha: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PullRequestReviewState {
    pub requested_reviewers: Vec<String>,
    pub latest_review_per_user: HashMap<String, String>,
    pub last_review_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PullRequestHeadAndAuthor {
    pub head_sha: String,
    pub author: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RepositorySettingsInput {
    pub repo: String,
    pub cwd: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RepositorySettings {
    pub allow_squash_merge: bool,
    pub allow_merge_commit: bool,
    pub allow_rebase_merge: bool,
    pub allow_auto_merge: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BranchProtectionInput {
    pub repo: String,
    pub branch: String,
    pub cwd: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BranchProtection {
    pub enabled: bool,
    pub has_required_checks: bool,
    pub required_checks: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IssueCommentInput {
    pub repo: String,
    pub issue_number: i64,
    pub body: String,
    pub cwd: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct IssueAssigneesInput {
    pub repo: String,
    pub issue_number: i64,
    pub assignees: Vec<String>,
    pub cwd: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct IssueLabelsInput {
    pub repo: String,
    pub issue_number: i64,
    pub labels: Vec<String>,
    pub cwd: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IssueCommentResult {
    pub id: i64,
    pub url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpdateIssueCommentInput {
    pub repo: String,
    pub comment_id: i64,
    pub body: String,
    pub cwd: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DeleteIssueCommentInput {
    pub repo: String,
    pub comment_id: i64,
    pub cwd: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CloseIssueInput {
    pub repo: String,
    pub issue_number: i64,
    pub state_reason: String,
    pub cwd: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ClosePullRequestInput {
    pub repo: String,
    pub pr_number: i64,
    pub cwd: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct MarkPullRequestReadyForReviewInput {
    pub repo: String,
    pub pr_number: i64,
    pub cwd: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct EnableAutoMergeInput {
    pub repo: String,
    pub pr_number: i64,
    pub strategy: String,
    pub head_sha: String,
    pub cwd: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PullRequestCheckRunsInput {
    pub repo: String,
    pub r#ref: String,
    pub cwd: String,
}

// ---------------------------------------------------------------------------
// Review types
// ---------------------------------------------------------------------------

/// Input for submitting a PR review.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SubmitReviewInput {
    pub repo: String,
    pub pr_number: i64,
    pub event: String,
    pub body: String,
    pub commit_id: String,
    pub comments: Vec<ReviewComment>,
    /// Anchor validation for review comments (placeholder for diffanchor::Index).
    pub anchors: Option<Value>,
    pub disclosure: DisclosureConfig,
    pub cwd: String,
}

/// A single inline review comment.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ReviewComment {
    pub body: String,
    pub path: String,
    pub line: i64,
    pub side: String,
    pub start_line: i64,
    pub start_side: String,
    pub diagnostic_index: i32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VerifyReviewMarkerInput {
    pub repo: String,
    pub pr_number: i64,
    pub marker: String,
    pub allowed_review_events: Vec<String>,
    pub author_login: String,
    pub allow_clean_comment: bool,
    pub cwd: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ReviewMarkerResult {
    pub found: bool,
    pub outcome: String,
    pub event: String,
    pub author_login: String,
    pub body: String,
    pub review_id: String,
    pub inline_comment_bodies: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PullRequestReactionInput {
    pub repo: String,
    pub pr_number: i64,
    pub content: String,
    pub cwd: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PullRequestCommentInput {
    pub repo: String,
    pub pr_number: i64,
    pub body: String,
    pub cwd: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PullRequestLabelsInput {
    pub repo: String,
    pub pr_number: i64,
    pub labels: Vec<String>,
    pub cwd: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PullRequestReviewersInput {
    pub repo: String,
    pub pr_number: i64,
    pub reviewers: Vec<String>,
    pub cwd: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CreatePullRequestInput {
    pub repo: String,
    pub head_branch: String,
    pub base_branch: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
    pub cwd: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CreatePullRequestResult {
    pub number: i64,
    pub url: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CompareBranchesInput {
    pub repo: String,
    pub base_branch: String,
    pub head_branch: String,
    pub cwd: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CompareBranchesResult {
    pub ahead_by: i32,
    pub behind_by: i32,
    pub status: String,
    pub total_commits: i32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct UpdatePullRequestTitleInput {
    pub repo: String,
    pub pr_number: i64,
    pub title: String,
    pub cwd: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct UpdatePullRequestBodyInput {
    pub repo: String,
    pub pr_number: i64,
    pub body: String,
    pub cwd: String,
}

// ---------------------------------------------------------------------------
// List inputs
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ListOpenPullRequestsInput {
    pub repo: String,
    pub cwd: String,
    pub limit: i32,
    pub label: String,
    pub labels: Vec<String>,
    pub author: String,
    pub base_ref_name: String,
    pub timeout: Option<std::time::Duration>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ListReviewRequestedPullRequestsInput {
    pub repo: String,
    pub cwd: String,
    pub limit: i32,
    pub reviewer: String,
    pub timeout: Option<std::time::Duration>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ListOpenIssuesInput {
    pub repo: String,
    pub cwd: String,
    pub limit: i32,
    pub assignee: String,
    pub label: String,
    pub labels: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ViewIssueInput {
    pub repo: String,
    pub issue_number: i64,
    pub cwd: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ViewPullRequestInput {
    pub repo: String,
    pub pr_number: i64,
    pub cwd: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ResolveReviewThreadInput {
    pub repo: String,
    pub thread_id: String,
    pub cwd: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ListReviewThreadsInput {
    pub repo: String,
    pub pr_number: i64,
    pub cwd: String,
    pub limit: i32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ViewReviewThreadInput {
    pub thread_id: String,
    pub cwd: String,
}

// ---------------------------------------------------------------------------
// Review thread types
// ---------------------------------------------------------------------------

/// A review thread with comments.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ReviewThread {
    pub id: String,
    pub is_resolved: bool,
    pub path: String,
    pub line: i64,
    pub url: String,
    pub comments: Vec<ReviewThreadComment>,
}

/// A single comment in a review thread.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ReviewThreadComment {
    pub id: String,
    pub body: String,
    pub author: String,
    pub author_association: String,
    pub created_at: String,
    pub updated_at: String,
    pub path: String,
    pub line: i64,
    pub original_commit_oid: String,
    pub commit_oid: String,
    pub url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AddReviewThreadReplyInput {
    pub repo: String,
    pub thread_id: String,
    pub body: String,
    pub cwd: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CompareCommitsInput {
    pub repo: String,
    pub base: String,
    pub head: String,
    pub cwd: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CompareCommitsResult {
    pub status: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct GetPullRequestDiffInput {
    pub repo: String,
    pub pr_number: i64,
    pub cwd: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CapturePullRequestSnapshotInput {
    pub project_id: String,
    pub repo: String,
    pub pr_number: i64,
    pub cwd: String,
    pub captured_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct InitializeLabelsInput {
    pub repo: String,
    pub cwd: String,
    pub dry_run: bool,
}

// ---------------------------------------------------------------------------
// Label types
// ---------------------------------------------------------------------------

/// Definition of a single repository label.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LabelDefinition {
    pub name: String,
    pub color: String,
    pub description: String,
}

/// Result of label initialization.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LabelInitResult {
    pub repo: String,
    pub dry_run: bool,
    pub labels: Vec<LabelInitItem>,
    pub summary: LabelInitSummary,
}

/// Status for a single label during init.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LabelInitItem {
    pub name: String,
    pub status: String,
    pub color: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Summary counts for label initialization.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LabelInitSummary {
    pub created: i32,
    pub updated: i32,
    pub skipped: i32,
    pub failed: i32,
}

/// Input for issue/implicit reactions.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ReviewThreadNotFoundError {
    pub thread_id: String,
}

// ---------------------------------------------------------------------------
// Snapshot record
// ---------------------------------------------------------------------------

/// Snapshot of a pull request at a point in time (bridge to looper-storage).
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PullRequestSnapshotRecord {
    pub project_id: String,
    pub repo: String,
    pub pr_number: i64,
    pub pr_title: String,
    pub pr_body: String,
    pub pr_head_sha: String,
    pub pr_base_sha: String,
    pub diff: String,
    pub captured_at: String,
}

// ---------------------------------------------------------------------------
// Internal types (not exported from Go, but used internally)
// ---------------------------------------------------------------------------

/// Idempotency marker parsed from a review body.
#[derive(Clone, Debug)]
pub struct ReviewIdempotencyMarker {
    pub id: String,
    #[allow(dead_code)]
    pub head: String,
    pub outcome: String,
}

/// GitHub reaction (private).
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct GitHubReaction {
    pub id: i64,
    pub content: String,
    pub user_login: String,
}

/// Review thread node (private).
#[derive(Clone, Debug)]
pub struct ReviewThreadNode {
    #[allow(dead_code)]
    pub id: String,
    #[allow(dead_code)]
    pub is_resolved: bool,
}

/// HTTP response from review submit (private).
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct ReviewSubmitHttpResponse {
    pub status_code: i32,
    pub headers: std::collections::HashMap<String, Value>,
    pub body: String,
}

/// Processing summary for review comments (private).
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct ReviewCommentProcessing {
    pub original_count: usize,
    pub submitted_count: usize,
    pub normalized_count: usize,
    pub downgraded_count: usize,
    pub dropped_count: usize,
    pub comments: Vec<HashMap<String, Value>>,
}

// ---------------------------------------------------------------------------
// Standard looper labels
// ---------------------------------------------------------------------------

/// Returns the set of labels managed by looper.
pub fn standard_looper_labels() -> Vec<LabelDefinition> {
    vec![
        LabelDefinition {
            name: "looper:plan".into(),
            color: "C5DEF5".into(),
            description: "Looper has created a plan for this issue".into(),
        },
        LabelDefinition {
            name: "looper:spec-reviewing".into(),
            color: "FBCA04".into(),
            description: "Looper is reviewing the specification".into(),
        },
        LabelDefinition {
            name: "looper:spec-ready".into(),
            color: "0E8A16".into(),
            description: "Specification is ready for implementation".into(),
        },
        LabelDefinition {
            name: "looper:needs-human".into(),
            color: "B60205".into(),
            description: "This issue needs human intervention".into(),
        },
    ]
}
