#![allow(clippy::type_complexity)]
//! GitHub gateway wrapping `gh` CLI commands and the GitHub REST/GraphQL API.
//!
//! # Overview
//!
//! The [`Gateway`] struct provides a high-level interface to GitHub operations:
//! pull requests, issues, reviews, labels, authentication, and discovery caches.
//! All operations delegate to the `gh` CLI binary or the GitHub API directly.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use looper_github::gateway::{Gateway, GatewayOptions};
//!
//! let gateway = Gateway::new(GatewayOptions::default());
//! let result = gateway.detect_current_repository(".");
//! ```

pub mod cache;
pub mod error;
pub mod gateway;
pub mod graphql;
pub mod helpers;
pub mod types;

pub use error::{error_message, GitHubError, TransientError};
pub use types::{
    BranchProtection, CapturePullRequestSnapshotInput, CommentInfo, CompareBranchesInput,
    CompareBranchesResult, CreatePullRequestInput, CreatePullRequestResult, CurrentUserIdentity,
    DependencyIssue, EnableAutoMergeInput, GetPullRequestDiffInput, GitHubUser, IssueCommentInput,
    IssueCommentResult, IssueDependency, IssueDetail, IssueReaction, IssueRepository, IssueState,
    IssueSummary, LabelDefinition, LabelInitItem, LabelInitResult, LabelInitSummary,
    LinkedPullRequest, PullRequestAutoMerge, PullRequestCheckRun, PullRequestCheckRuns,
    PullRequestCheckRunsInput, PullRequestCommentInput, PullRequestDetail, PullRequestHeadAndAuthor,
    PullRequestLabelsInput, PullRequestReactionInput, PullRequestReviewState,
    PullRequestReviewersInput, PullRequestSnapshotRecord, PullRequestStatus, PullRequestSummary,
    RepositorySettings, ReviewComment, ReviewMarkerResult, ReviewThread, ReviewThreadComment,
    SubmitReviewInput, UpdatePullRequestBodyInput, UpdatePullRequestTitleInput,
    VerifyReviewMarkerInput, ViewPullRequestInput, standard_looper_labels,
};
pub use gateway::Gateway;

#[cfg(test)]
mod tests;
