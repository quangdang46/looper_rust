//! Shared types for forge operations.

use serde::{Deserialize, Serialize};

/// An issue on a forge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    pub state: IssueState,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub author: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueState {
    Open,
    Closed,
}

/// Filter for listing issues.
#[derive(Debug, Clone, Default)]
pub struct IssueFilter {
    pub state: Option<IssueState>,
    pub labels: Option<Vec<String>>,
    pub assignee: Option<String>,
    pub limit: Option<u32>,
}

/// A pull request on a forge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    pub state: PrState,
    pub head_sha: String,
    pub base_branch: String,
    pub head_branch: String,
    pub author: String,
    pub labels: Vec<String>,
    pub mergeable: Option<bool>,
    pub merged: bool,
    pub merged_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    Open,
    Closed,
    Merged,
}

/// Filter for listing pull requests.
#[derive(Debug, Clone, Default)]
pub struct PrFilter {
    pub state: Option<PrState>,
    pub labels: Option<Vec<String>>,
    pub limit: Option<u32>,
}

/// Input for creating a pull request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatePr {
    pub title: String,
    pub body: Option<String>,
    pub head: String,
    pub base: String,
}

/// Merge strategy for pull requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeStrategy {
    Squash,
    Merge,
    Rebase,
}

/// Input for submitting a review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewInput {
    pub event: ReviewEvent,
    pub body: Option<String>,
    pub comments: Vec<ReviewComment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewEvent {
    Approve,
    RequestChanges,
    Comment,
}

/// A comment on a specific file/line in a review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewComment {
    pub path: String,
    pub line: Option<u32>,
    pub body: String,
}

/// A comment on an issue or PR.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub id: u64,
    pub author: String,
    pub body: String,
    pub created_at: String,
}

/// A review thread (conversation on a specific line).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewThread {
    pub id: String,
    pub resolved: bool,
    pub comments: Vec<Comment>,
    pub file_path: Option<String>,
    pub line: Option<u32>,
}
