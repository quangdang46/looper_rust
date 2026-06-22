//! GitHub Gateway — wraps `gh` CLI commands and the GitHub REST/GraphQL API.

use std::{
    collections::HashMap,
    sync::Arc,
    time::Duration,
};

use serde_json::Value;

use crate::cache::DiscoveryCache;
use crate::error::*;
use crate::helpers::*;
use crate::types::*;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const DEFAULT_GH_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
const PR_DIFF_GH_COMMAND_TIMEOUT: Duration = Duration::from_secs(180);
const PR_DISCOVERY_CACHE_TTL: Duration = Duration::from_secs(30);


const PR_LIST_JSON_FIELDS: &[&str] = &[
    "number", "title", "url", "state", "updatedAt", "isDraft", "reviewDecision",
    "labels", "headRefName", "baseRefName", "headRefOid", "baseRefOid",
    "author", "reviewRequests", "reviews", "mergeStateStatus",
];

const PR_VIEW_JSON_FIELDS: &[&str] = &[
    "number", "title", "body", "url", "state", "createdAt", "updatedAt",
    "closedAt", "isDraft", "reviewDecision", "labels", "headRefName", "baseRefName",
    "headRefOid", "baseRefOid", "author", "reviewRequests", "comments",
    "reviews", "statusCheckRollup", "mergeStateStatus",
];

// ---------------------------------------------------------------------------
// Shell execution types
// ---------------------------------------------------------------------------

/// Result from running a shell command.
pub struct ShellResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Options for running a shell command.
pub struct ShellOptions {
    pub cwd: String,
    pub stdin: Option<String>,
    pub timeout: Duration,
    pub args: Vec<String>,
}

// ---------------------------------------------------------------------------
// GatewayOptions
// ---------------------------------------------------------------------------

/// Options for constructing a Gateway.
pub struct GatewayOptions {
    pub gh_path: String,
    pub cwd: String,
    pub discovery_cache_ttl: Duration,
    /// Custom gh runner. Defaults to spawning the gh binary.
    pub gh_run: Option<Arc<dyn Fn(ShellOptions) -> Result<ShellResult, GitHubError> + Send + Sync>>,
    /// Optional diagnostic callback for review submission.
    pub review_submit_diagnostic:
        Option<Arc<dyn Fn(String, HashMap<String, Value>) + Send + Sync>>,
}

impl Default for GatewayOptions {
    fn default() -> Self {
        Self {
            gh_path: "gh".to_string(),
            cwd: ".".to_string(),
            discovery_cache_ttl: PR_DISCOVERY_CACHE_TTL,
            gh_run: None,
            review_submit_diagnostic: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Gateway
// ---------------------------------------------------------------------------

/// Gateway to the GitHub API via the `gh` CLI.
pub struct Gateway {
    pub gh_path: String,
    pub cwd: String,
    pub discovery_cache_ttl: Duration,
    pub discovery_cache: Arc<DiscoveryCache>,
    gh_run:
        Arc<dyn Fn(ShellOptions) -> Result<ShellResult, GitHubError> + Send + Sync>,
    review_submit_diagnostic:
        Option<Arc<dyn Fn(String, HashMap<String, Value>) + Send + Sync>>,
}

impl Gateway {
    // -----------------------------------------------------------------------
    // Constructor
    // -----------------------------------------------------------------------

    pub fn new(options: GatewayOptions) -> Self {
        let gh_run = options.gh_run.unwrap_or_else(|| {
            let gh_path = options.gh_path.clone();
            Arc::new(move |opts: ShellOptions| -> Result<ShellResult, GitHubError> {
                run_gh_command(&gh_path, &opts)
            })
        });

        Self {
            gh_path: options.gh_path,
            cwd: options.cwd,
            discovery_cache_ttl: options.discovery_cache_ttl,
            discovery_cache: Arc::new(DiscoveryCache::new()),
            gh_run,
            review_submit_diagnostic: options.review_submit_diagnostic,
        }
    }

    // -----------------------------------------------------------------------
    // Internal helpers — gh command execution
    // -----------------------------------------------------------------------

    fn run_gh(&self, cwd: &str, stdin: &str, args: &[&str]) -> Result<ShellResult, GitHubError> {
        self.run_gh_with_timeout(cwd, stdin, DEFAULT_GH_COMMAND_TIMEOUT, args)
    }

    fn run_gh_with_timeout(
        &self,
        cwd: &str,
        stdin: &str,
        timeout: Duration,
        args: &[&str],
    ) -> Result<ShellResult, GitHubError> {
        let opts = ShellOptions {
            cwd: cwd.to_string(),
            stdin: if stdin.is_empty() {
                None
            } else {
                Some(stdin.to_string())
            },
            timeout,
            args: args.iter().map(|s| s.to_string()).collect(),
        };
        (self.gh_run)(opts)
    }

    fn emit_diagnostic(&self, stage: &str, data: HashMap<String, Value>) {
        if let Some(ref diag) = self.review_submit_diagnostic {
            diag(stage.to_string(), data);
        }
    }

    // -----------------------------------------------------------------------
    // DISCOVERY / LISTING
    // -----------------------------------------------------------------------

    pub fn list_open_pull_requests(
        &self,
        input: ListOpenPullRequestsInput,
    ) -> Result<Vec<PullRequestSummary>, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let limit = default_limit(input.limit);

        // Build cache key
        let cache_key = format!(
            "{}|{}|{}|{}",
            input.repo, input.label, input.author, input.base_ref_name
        );

        // Check cache
        let ttl = input.timeout.unwrap_or(self.discovery_cache_ttl);
        if let Some(cached) = self.discovery_cache.get_prs(&cache_key, ttl) {
            return Ok(cached);
        }

        let limit_str = limit.to_string();
        let fields_str = PR_LIST_JSON_FIELDS.join(",");
        let mut args = vec![
            "pr",
            "list",
            "--repo",
            &input.repo,
            "--state",
            "open",
            "--limit",
            &limit_str,
            "--json",
            &fields_str,
        ];

        if !input.label.is_empty() {
            args.push("--label");
            args.push(&input.label);
        }
        for label in &input.labels {
            args.push("--label");
            args.push(label);
        }
        if !input.author.is_empty() {
            args.push("--author");
            args.push(&input.author);
        }
        if !input.base_ref_name.is_empty() {
            args.push("--base");
            args.push(&input.base_ref_name);
        }

        let result = self.run_gh(cwd, "", &args)?;
        let items: Vec<HashMap<String, Value>> =
            decode_json_array(&result.stdout).map_err(GitHubError::JsonParse)?;
        let summaries: Vec<PullRequestSummary> =
            items.into_iter().map(parse_pr_summary).collect();

        self.discovery_cache
            .set_prs(cache_key, summaries.clone(), ttl);
        Ok(summaries)
    }

    pub fn list_review_requested_pull_requests(
        &self,
        input: ListReviewRequestedPullRequestsInput,
    ) -> Result<Vec<PullRequestSummary>, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let limit = default_limit(input.limit);
        let cache_key = format!("review|{}|{}", input.repo, input.reviewer);

        let ttl = input.timeout.unwrap_or(self.discovery_cache_ttl);
        if let Some(cached) = self.discovery_cache.get_review_prs(&cache_key, ttl) {
            return Ok(cached);
        }

        let query = format!(
            "repo:{} is:pr is:open review-requested:{}",
            input.repo, input.reviewer
        );
        let result = self.run_gh_graphql(
            cwd,
            crate::graphql::SEARCH_PRS_BY_REVIEW_REQUESTED_QUERY,
            &[
                ("searchQuery", Value::String(query)),
                ("first", Value::Number(limit.into())),
            ],
        )?;

        let items = parse_search_pr_nodes(&result)?;
        let summaries: Vec<PullRequestSummary> =
            items.into_iter().map(parse_pr_summary).collect();

        self.discovery_cache
            .set_review_prs(cache_key, summaries.clone(), ttl);
        Ok(summaries)
    }

    pub fn list_open_issues(
        &self,
        input: ListOpenIssuesInput,
    ) -> Result<Vec<IssueSummary>, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let limit = default_limit(input.limit);
        let cache_key = format!(
            "issues|{}|{}|{}",
            input.repo, input.assignee, input.label
        );

        let ttl = self.discovery_cache_ttl;
        if let Some(cached) = self.discovery_cache.get_issues(&cache_key, ttl) {
            return Ok(cached);
        }

        let limit_str = limit.to_string();
        let mut args = vec![
            "issue",
            "list",
            "--repo",
            &input.repo,
            "--state",
            "open",
            "--limit",
            &limit_str,
            "--json",
            "number,title,body,url,state,updatedAt,author,assignees,labels",
        ];

        if !input.assignee.is_empty() {
            args.push("--assignee");
            args.push(&input.assignee);
        }
        if !input.label.is_empty() {
            args.push("--label");
            args.push(&input.label);
        }
        for label in &input.labels {
            args.push("--label");
            args.push(label);
        }

        let result = self.run_gh(cwd, "", &args)?;
        let items: Vec<HashMap<String, Value>> =
            decode_json_array(&result.stdout).map_err(GitHubError::JsonParse)?;
        let summaries: Vec<IssueSummary> = items.into_iter().map(parse_issue_summary).collect();

        self.discovery_cache
            .set_issues(cache_key, summaries.clone(), ttl);
        Ok(summaries)
    }

    // -----------------------------------------------------------------------
    // ISSUE VIEW / DEPENDENCIES
    // -----------------------------------------------------------------------

    pub fn view_issue(&self, input: ViewIssueInput) -> Result<IssueDetail, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };

        // Get issue details
        let result = self.run_gh(
            cwd,
            "",
            &[
                "api",
                &format!("repos/{}/issues/{}", input.repo, input.issue_number),
            ],
        )?;
        let issue_data: HashMap<String, Value> =
            decode_json_object(&result.stdout).map_err(GitHubError::JsonParse)?;

        // Get comments (paginated)
        let comments = self.list_issue_comments_internal(cwd, &input.repo, input.issue_number)?;

        Ok(parse_issue_detail(&issue_data, comments))
    }

    pub fn get_issue_state(&self, input: ViewIssueInput) -> Result<IssueState, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let result = self.run_gh(
            cwd,
            "",
            &[
                "api",
                &format!("repos/{}/issues/{}", input.repo, input.issue_number),
            ],
        )?;
        let data: HashMap<String, Value> =
            decode_json_object(&result.stdout).map_err(GitHubError::JsonParse)?;
        Ok(IssueState {
            state: data.get("state").map(as_string).unwrap_or_default(),
            state_reason: data
                .get("stateReason")
                .map(as_string)
                .unwrap_or_default(),
        })
    }

    pub fn list_issue_blocked_by(
        &self,
        input: ListIssueBlockedByInput,
    ) -> Result<Vec<IssueDependency>, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let result = self.run_gh(
            cwd,
            "",
            &[
                "api",
                "--paginate",
                &format!(
                    "repos/{}/issues/{}/dependencies/blocked_by",
                    input.repo, input.issue_number
                ),
            ],
        )?;
        let items: Vec<HashMap<String, Value>> =
            decode_json_array(&result.stdout).map_err(GitHubError::JsonParse)?;
        Ok(items
            .into_iter()
            .map(|m| IssueDependency {
                number: m.get("number").map(as_i64).unwrap_or(0),
                repo: input.repo.clone(),
            })
            .collect())
    }

    pub fn list_issue_comments(&self, input: ViewIssueInput) -> Result<Vec<CommentInfo>, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        self.list_issue_comments_internal(cwd, &input.repo, input.issue_number)
    }

    fn list_issue_comments_internal(
        &self,
        cwd: &str,
        repo: &str,
        issue_number: i64,
    ) -> Result<Vec<CommentInfo>, GitHubError> {
        let result = self.run_gh(
            cwd,
            "",
            &[
                "api",
                "--paginate",
                "--slurp",
                &format!("repos/{}/issues/{}/comments", repo, issue_number),
            ],
        )?;
        let items: Vec<HashMap<String, Value>> =
            decode_json_array_or_pages(&result.stdout).map_err(GitHubError::JsonParse)?;
        Ok(items.into_iter().map(|m| parse_comment_info(&m)).collect())
    }

    pub fn list_issue_timeline(
        &self,
        input: IssueTimelineInput,
    ) -> Result<Vec<HashMap<String, Value>>, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let result = self.run_gh(
            cwd,
            "",
            &[
                "api",
                "--paginate",
                "--slurp",
                &format!(
                    "repos/{}/issues/{}/timeline",
                    input.repo, input.issue_number
                ),
            ],
        )?;
        decode_json_array_or_pages(&result.stdout).map_err(GitHubError::JsonParse)
    }

    pub fn list_issue_reactions(
        &self,
        input: IssueReactionInput,
    ) -> Result<Vec<IssueReaction>, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let endpoint = if input.comment_id > 0 {
            format!(
                "repos/{}/issues/comments/{}/reactions",
                input.repo, input.comment_id
            )
        } else {
            format!(
                "repos/{}/issues/{}/reactions",
                input.repo, input.issue_number
            )
        };
        let result = self.run_gh(
            cwd,
            "",
            &["api", "--paginate", "--slurp", &endpoint],
        )?;
        let items: Vec<HashMap<String, Value>> =
            decode_json_array_or_pages(&result.stdout).map_err(GitHubError::JsonParse)?;
        Ok(items
            .into_iter()
            .map(|m| IssueReaction {
                id: m.get("id").map(as_i64).unwrap_or(0),
                content: m.get("content").map(as_string).unwrap_or_default(),
                user_login: nested_string(&m, &["user", "login"]),
            })
            .collect())
    }

    pub fn add_issue_reaction(&self, input: CreateIssueReactionInput) -> Result<(), GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let endpoint = if input.comment_id > 0 {
            format!(
                "repos/{}/issues/comments/{}/reactions",
                input.repo, input.comment_id
            )
        } else {
            format!(
                "repos/{}/issues/{}/reactions",
                input.repo, input.issue_number
            )
        };
        self.run_gh(
            cwd,
            "",
            &[
                "api",
                &endpoint,
                "--method",
                "POST",
                "-H",
                "Accept: application/vnd.github+json",
                "-f",
                &format!("content={}", input.content),
            ],
        )?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // ISSUE COMMENTS & MUTATIONS
    // -----------------------------------------------------------------------

    pub fn create_issue_comment(
        &self,
        input: IssueCommentInput,
    ) -> Result<IssueCommentResult, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let body_json = serde_json::json!({"body": input.body}).to_string();
        let result = self.run_gh(
            cwd,
            &body_json,
            &[
                "api",
                &format!(
                    "repos/{}/issues/{}/comments",
                    input.repo, input.issue_number
                ),
                "--method",
                "POST",
                "--input",
                "-",
            ],
        )?;
        let data: HashMap<String, Value> =
            decode_json_object(&result.stdout).map_err(GitHubError::JsonParse)?;
        Ok(IssueCommentResult {
            id: data.get("id").map(as_i64).unwrap_or(0),
            url: data.get("url").map(as_string).unwrap_or_default(),
        })
    }

    pub fn update_issue_comment(&self, input: UpdateIssueCommentInput) -> Result<(), GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let body_json = serde_json::json!({"body": input.body}).to_string();
        self.run_gh(
            cwd,
            &body_json,
            &[
                "api",
                &format!("repos/{}/issues/comments/{}", input.repo, input.comment_id),
                "--method",
                "PATCH",
                "--input",
                "-",
            ],
        )?;
        Ok(())
    }

    pub fn delete_issue_comment(&self, input: DeleteIssueCommentInput) -> Result<(), GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        self.run_gh(
            cwd,
            "",
            &[
                "api",
                &format!("repos/{}/issues/comments/{}", input.repo, input.comment_id),
                "--method",
                "DELETE",
            ],
        )?;
        Ok(())
    }

    pub fn close_issue(&self, input: CloseIssueInput) -> Result<(), GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        // Check current state first (idempotent)
        let state_result = self.get_issue_state(ViewIssueInput {
            repo: input.repo.clone(),
            issue_number: input.issue_number,
            cwd: cwd.to_string(),
        });
        if let Ok(state) = state_result {
            if state.state == "closed" {
                return Ok(());
            }
        }
        self.run_gh(
            cwd,
            "",
            &[
                "issue",
                "close",
                &input.issue_number.to_string(),
                "--repo",
                &input.repo,
                "--reason",
                &input.state_reason,
            ],
        )?;
        Ok(())
    }

    pub fn add_issue_assignees(&self, input: IssueAssigneesInput) -> Result<(), GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        // Build gh api -f assignees[]=<user> per assignee
        let _assignees: Vec<String> = input
            .assignees
            .iter()
            .map(|a| format!("assignees[]={}", a))
            .collect();
        let endpoint = format!(
            "repos/{}/issues/{}/assignees",
            input.repo, input.issue_number
        );
        let mut flag_args: Vec<String> = Vec::new();
        for a in &input.assignees {
            flag_args.push("-f".to_string());
            flag_args.push(format!("assignees[]={}", a));
        }
        let flag_refs: Vec<&str> = flag_args.iter().map(|s| s.as_str()).collect();
        let mut args: Vec<&str> = vec!["api", &endpoint, "--method", "POST"];
        args.extend(flag_refs);
        self.run_gh(cwd, "", &args)?;
        Ok(())
    }

    pub fn add_issue_labels(&self, input: IssueLabelsInput) -> Result<(), GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        // Ensure labels exist first
        let _ = self.ensure_labels_exist(cwd, &input.repo, &input.labels);
        let payload = serde_json::json!({"labels": input.labels}).to_string();
        self.run_gh(
            cwd,
            &payload,
            &[
                "api",
                &format!("repos/{}/issues/{}/labels", input.repo, input.issue_number),
                "--method",
                "POST",
                "--input",
                "-",
            ],
        )?;
        Ok(())
    }

    pub fn remove_issue_labels(&self, input: IssueLabelsInput) -> Result<(), GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        for label in &input.labels {
            let encoded = encode_uri_component(label);
            let _result = self.run_gh(
                cwd,
                "",
                &[
                    "api",
                    &format!(
                        "repos/{}/issues/{}/labels/{}",
                        input.repo, input.issue_number, encoded
                    ),
                    "--method",
                    "DELETE",
                ],
            );
            // Ignore per-label errors for idempotent removal
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // REPOSITORY INFO
    // -----------------------------------------------------------------------

    pub fn get_repository_permission(
        &self,
        input: RepositoryPermissionInput,
    ) -> Result<String, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let result = self.run_gh(
            cwd,
            "",
            &[
                "api",
                &format!(
                    "repos/{}/collaborators/{}/permission",
                    input.repo, input.user
                ),
            ],
        )?;
        let data: HashMap<String, Value> =
            decode_json_object(&result.stdout).map_err(GitHubError::JsonParse)?;
        Ok(data
            .get("permission")
            .map(as_string)
            .unwrap_or_default())
    }

    pub fn get_repository_settings(
        &self,
        input: RepositorySettingsInput,
    ) -> Result<RepositorySettings, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let result = self.run_gh(cwd, "", &["api", &format!("repos/{}", input.repo)])?;
        let data: HashMap<String, Value> =
            decode_json_object(&result.stdout).map_err(GitHubError::JsonParse)?;
        Ok(RepositorySettings {
            allow_squash_merge: as_bool(
                data.get("allow_squash_merge")
                    .unwrap_or(&Value::Bool(false)),
            ),
            allow_merge_commit: as_bool(
                data.get("allow_merge_commit")
                    .unwrap_or(&Value::Bool(false)),
            ),
            allow_rebase_merge: as_bool(
                data.get("allow_rebase_merge")
                    .unwrap_or(&Value::Bool(false)),
            ),
            allow_auto_merge: as_bool(
                data.get("allow_auto_merge")
                    .unwrap_or(&Value::Bool(false)),
            ),
        })
    }

    pub fn get_branch_protection(
        &self,
        input: BranchProtectionInput,
    ) -> Result<BranchProtection, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let result = self.run_gh(
            cwd,
            "",
            &[
                "api",
                &format!(
                    "repos/{}/branches/{}/protection",
                    input.repo, input.branch
                ),
            ],
        );
        match result {
            Ok(res) => {
                let data: HashMap<String, Value> =
                    decode_json_object(&res.stdout).map_err(GitHubError::JsonParse)?;
                let required_checks = if let Some(checks) =
                    data.get("required_status_checks")
                {
                    if let Some(checks_obj) = checks.as_object() {
                        checks_obj
                            .get("contexts")
                            .map(|c| {
                                c.as_array()
                                    .map(|arr| {
                                        arr.iter()
                                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                            .collect()
                                    })
                                    .unwrap_or_default()
                            })
                            .unwrap_or_default()
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                };
                Ok(BranchProtection {
                    enabled: true,
                    has_required_checks: !required_checks.is_empty(),
                    required_checks,
                })
            }
            Err(_) => Ok(BranchProtection {
                enabled: false,
                has_required_checks: false,
                required_checks: vec![],
            }),
        }
    }

    // -----------------------------------------------------------------------
    // PULL REQUESTS — VIEW / DETAIL
    // -----------------------------------------------------------------------

    pub fn view_pull_request(
        &self,
        input: ViewPullRequestInput,
    ) -> Result<PullRequestDetail, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let result = self.run_gh(
            cwd,
            "",
            &[
                "pr",
                "view",
                &input.pr_number.to_string(),
                "--repo",
                &input.repo,
                "--json",
                &PR_VIEW_JSON_FIELDS.join(","),
            ],
        )?;
        let data: HashMap<String, Value> =
            decode_json_object(&result.stdout).map_err(GitHubError::JsonParse)?;
        parse_pr_detail(&data)
    }

    #[allow(clippy::field_reassign_with_default)]
    pub fn view_pull_request_merge_watch(
        &self,
        input: ViewPullRequestInput,
    ) -> Result<PullRequestDetail, GitHubError> {
        // Use REST API for merge status specifically
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let result = self.run_gh(
            cwd,
            "",
            &[
                "api",
                &format!("repos/{}/pulls/{}", input.repo, input.pr_number),
            ],
        )?;
        let data: HashMap<String, Value> =
            decode_json_object(&result.stdout).map_err(GitHubError::JsonParse)?;
        // Wrap in a detail struct with available data
        let mut detail = PullRequestDetail::default();
        detail.number = data.get("number").map(as_i64).unwrap_or(0);
        detail.title = data.get("title").map(as_string).unwrap_or_default();
        detail.body = data.get("body").map(as_string).unwrap_or_default();
        detail.url = data.get("html_url").map(as_string).unwrap_or_default();
        detail.state = data.get("state").map(as_string).unwrap_or_default();
        detail.mergeable = data
            .get("mergeable")
            .and_then(|v| match v {
                Value::Bool(b) => Some(*b),
                Value::Null => None,
                _ => None,
            });
        detail.mergeable_state =
            data.get("mergeable_state").map(as_string).unwrap_or_default();
        detail.merged_at = data.get("merged_at").map(as_string).unwrap_or_default();
        detail.head_sha = nested_string(&data, &["head", "sha"]);
        detail.base_sha = nested_string(&data, &["base", "sha"]);
        Ok(detail)
    }

    pub fn get_pull_request_author(
        &self,
        input: ViewPullRequestInput,
    ) -> Result<String, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let result = self.run_gh(
            cwd,
            "",
            &[
                "pr",
                "view",
                &input.pr_number.to_string(),
                "--repo",
                &input.repo,
                "--json",
                "author",
                "--jq",
                ".author.login",
            ],
        )?;
        Ok(result.stdout.trim().to_string())
    }

    pub fn get_pull_request_head_and_author(
        &self,
        input: ViewPullRequestInput,
    ) -> Result<PullRequestHeadAndAuthor, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let result = self.run_gh(
            cwd,
            "",
            &[
                "pr",
                "view",
                &input.pr_number.to_string(),
                "--repo",
                &input.repo,
                "--json",
                "headRefOid,author",
            ],
        )?;
        let data: HashMap<String, Value> =
            decode_json_object(&result.stdout).map_err(GitHubError::JsonParse)?;
        Ok(PullRequestHeadAndAuthor {
            head_sha: data
                .get("headRefOid")
                .map(as_string)
                .unwrap_or_default(),
            author: extract_author(
                data.get("author").unwrap_or(&Value::Null),
            ),
        })
    }

    pub fn list_pull_request_check_runs(
        &self,
        input: PullRequestCheckRunsInput,
    ) -> Result<PullRequestCheckRuns, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        // Get check runs
        let check_result = self.run_gh(
            cwd,
            "",
            &[
                "api",
                &format!("repos/{}/commits/{}/check-runs", input.repo, input.r#ref),
            ],
        );
        // Get commit status
        let status_result = self.run_gh(
            cwd,
            "",
            &[
                "api",
                &format!("repos/{}/commits/{}/status", input.repo, input.r#ref),
            ],
        );

        let mut check_runs = Vec::new();
        let mut total_count = 0;
        if let Ok(res) = check_result {
            if let Ok(data) = decode_json_object(&res.stdout) {
                total_count = data.get("total_count").map(as_i64).unwrap_or(0) as i32;
                if let Some(runs) = data.get("check_runs") {
                    if let Some(arr) = runs.as_array() {
                        check_runs = arr
                            .iter()
                            .filter_map(|v| {
                                v.as_object().map(|m| PullRequestCheckRun {
                                    name: m
                                        .get("name")
                                        .map(as_string)
                                        .unwrap_or_default(),
                                    status: m
                                        .get("status")
                                        .map(as_string)
                                        .unwrap_or_default(),
                                    conclusion: m
                                        .get("conclusion")
                                        .map(as_string)
                                        .unwrap_or_default(),
                                })
                            })
                            .collect();
                    }
                }
            }
        }

        let mut statuses = Vec::new();
        if let Ok(res) = status_result {
            if let Ok(data) = decode_json_object(&res.stdout) {
                if let Some(s) = data.get("statuses") {
                    if let Some(arr) = s.as_array() {
                        statuses = arr
                            .iter()
                            .filter_map(|v| {
                                v.as_object().map(|m| PullRequestStatus {
                                    context: m
                                        .get("context")
                                        .map(as_string)
                                        .unwrap_or_default(),
                                    state: m
                                        .get("state")
                                        .map(as_string)
                                        .unwrap_or_default(),
                                })
                            })
                            .collect();
                    }
                }
            }
        }

        Ok(PullRequestCheckRuns {
            total_count,
            check_runs,
            statuses,
        })
    }

    // -----------------------------------------------------------------------
    // LINKED PRs / REVIEW STATE
    // -----------------------------------------------------------------------

    pub fn list_linked_pull_requests(
        &self,
        input: LinkedPullRequestsInput,
    ) -> Result<Vec<LinkedPullRequest>, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let (owner, name) = split_repo_owner_name(&input.repo);
        let variables = serde_json::json!({
            "owner": owner,
            "repo": name,
            "number": input.issue_number,
            "after": "",
        });
        let result = self.run_gh_graphql(
            cwd,
            crate::graphql::LINKED_PULL_REQUESTS_QUERY,
            &[
                ("owner", variables["owner"].clone()),
                ("repo", variables["repo"].clone()),
                ("number", variables["number"].clone()),
                ("after", Value::Null),
            ],
        )?;
        // Parse from response
        let mut linked = Vec::new();
        if let Some(data) = result.as_object() {
            if let Some(repo) = data.get("repository") {
                if let Some(issue) = repo.as_object().and_then(|r| r.get("issue")) {
                    if let Some(pr_refs) = issue.as_object().and_then(|i| i.get("closedByPullRequestsReferences")) {
                        if let Some(nodes) = pr_refs.as_object().and_then(|p| p.get("nodes")) {
                            if let Some(arr) = nodes.as_array() {
                                for node in arr {
                                    if let Some(obj) = node.as_object() {
                                        linked.push(LinkedPullRequest {
                                            number: obj.get("number").map(as_i64).unwrap_or(0),
                                            state: obj.get("state").map(as_string).unwrap_or_default(),
                                            merged: obj.get("merged").map(as_bool).unwrap_or(false),
                                            merged_at: obj.get("mergedAt").map(as_string).unwrap_or_default(),
                                            merge_commit_sha: obj
                        .get("mergeCommit")
                        .and_then(|v| v.as_object())
                        .and_then(|m| m.get("oid"))
                        .map(as_string)
                        .unwrap_or_default(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(linked)
    }

    pub fn list_pull_request_review_state(
        &self,
        input: PullRequestReviewStateInput,
    ) -> Result<PullRequestReviewState, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let result = self.run_gh(
            cwd,
            "",
            &[
                "pr",
                "view",
                &input.pr_number.to_string(),
                "--repo",
                &input.repo,
                "--json",
                "reviewRequests,reviews",
            ],
        )?;
        let data: HashMap<String, Value> =
            decode_json_object(&result.stdout).map_err(GitHubError::JsonParse)?;

        let requested_reviewers = data
            .get("reviewRequests")
            .map(extract_review_request_logins)
            .unwrap_or_default();

        let reviews_arr = to_object_slice(data.get("reviews").unwrap_or(&Value::Null));
        let mut latest_review: HashMap<String, String> = HashMap::new();
        let mut last_review_at = String::new();

        for review in &reviews_arr {
            if let Some(author) = review
                .get("author")
                .and_then(|a| a.as_object())
                .and_then(|a| a.get("login"))
                .map(as_string)
            {
                if let Some(state) = review.get("state").map(as_string) {
                    if !state.is_empty() && state != "PENDING" {
                        latest_review.insert(author, review_event_from_state(&state).to_string());
                    }
                }
            }
            let submitted_at = review.get("submittedAt").map(as_string).unwrap_or_default();
            if submitted_at > last_review_at {
                last_review_at = submitted_at;
            }
        }

        Ok(PullRequestReviewState {
            requested_reviewers,
            latest_review_per_user: latest_review,
            last_review_at,
        })
    }

    pub fn close_pull_request(&self, input: ClosePullRequestInput) -> Result<(), GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        self.run_gh(
            cwd,
            "",
            &[
                "pr",
                "close",
                &input.pr_number.to_string(),
                "--repo",
                &input.repo,
            ],
        )?;
        Ok(())
    }

    pub fn enable_auto_merge(&self, input: EnableAutoMergeInput) -> Result<(), GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        self.run_gh(
            cwd,
            "",
            &[
                "pr",
                "merge",
                &input.pr_number.to_string(),
                "--repo",
                &input.repo,
                "--auto",
                "--match-head-commit",
                &input.head_sha,
            ],
        )?;
        Ok(())
    }

    pub fn get_pull_request_head_sha(
        &self,
        input: ViewPullRequestInput,
    ) -> Result<String, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let result = self.run_gh(
            cwd,
            "",
            &[
                "pr",
                "view",
                &input.pr_number.to_string(),
                "--repo",
                &input.repo,
                "--json",
                "headRefOid",
                "--jq",
                ".headRefOid",
            ],
        )?;
        Ok(result.stdout.trim().to_string())
    }

    // -----------------------------------------------------------------------
    // REVIEW THREADS (GraphQL)
    // -----------------------------------------------------------------------

    pub fn resolve_review_thread(&self, input: ResolveReviewThreadInput) -> Result<(), GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let variables = serde_json::json!({"threadId": input.thread_id});
        self.run_gh_graphql(
            cwd,
            crate::graphql::RESOLVE_REVIEW_THREAD_MUTATION,
            &[("threadId", variables["threadId"].clone())],
        )?;
        Ok(())
    }

    pub fn view_review_thread(
        &self,
        input: ViewReviewThreadInput,
    ) -> Result<ReviewThread, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let variables = serde_json::json!({"threadId": input.thread_id, "after": ""});
        let result = self.run_gh_graphql(
            cwd,
            crate::graphql::REVIEW_THREAD_QUERY,
            &[
                ("threadId", variables["threadId"].clone()),
                ("after", Value::Null),
            ],
        )?;

        // Parse the response
        let mut comments = Vec::new();
        let id = String::new();
        if let Some(data) = result.as_object() {
            if let Some(node) = data.get("node") {
                if let Some(comments_conn) = node.as_object().and_then(|n| n.get("comments")) {
                    if let Some(nodes) = comments_conn.as_object().and_then(|c| c.get("nodes")) {
                        if let Some(arr) = nodes.as_array() {
                            append_review_thread_comment(&mut comments, arr);
                        }
                    }
                }
            }
        }

        Ok(ReviewThread {
            id,
            is_resolved: false,
            path: String::new(),
            line: 0,
            url: String::new(),
            comments,
        })
    }

    pub fn list_review_threads(
        &self,
        input: ListReviewThreadsInput,
    ) -> Result<Vec<ReviewThread>, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let (owner, name) = split_repo_owner_name(&input.repo);
        let variables = serde_json::json!({
            "owner": owner,
            "name": name,
            "prNumber": input.pr_number,
            "limit": default_limit(input.limit),
            "after": "",
        });
        let result = self.run_gh_graphql(
            cwd,
            crate::graphql::REVIEW_THREADS_QUERY,
            &[
                ("owner", variables["owner"].clone()),
                ("name", variables["name"].clone()),
                ("prNumber", variables["prNumber"].clone()),
                ("limit", variables["limit"].clone()),
                ("after", Value::Null),
            ],
        )?;

        let mut threads = Vec::new();
        if let Some(data) = result.as_object() {
            if let Some(repo) = data.get("repository") {
                if let Some(pr) = repo.as_object().and_then(|r| r.get("pullRequest")) {
                    if let Some(thread_conn) = pr.as_object().and_then(|p| p.get("reviewThreads")) {
                        if let Some(nodes) = thread_conn.as_object().and_then(|t| t.get("nodes")) {
                            if let Some(arr) = nodes.as_array() {
                                for node_val in arr {
                                    if let Some(node) = node_val.as_object() {
                                        let mut comments = Vec::new();
                                        if let Some(comments_conn) = node.get("comments") {
                                            if let Some(comments_nodes) = comments_conn.as_object().and_then(|c| c.get("nodes")) {
                                                if let Some(arr) = comments_nodes.as_array() {
                                                    append_review_thread_comment(&mut comments, arr);
                                                }
                                            }
                                        }
                                        threads.push(ReviewThread {
                                            id: node.get("id").map(as_string).unwrap_or_default(),
                                            is_resolved: node.get("isResolved").map(as_bool).unwrap_or(false),
                                            path: node.get("path").map(as_string).unwrap_or_default(),
                                            line: node.get("line").map(as_i64).unwrap_or(0),
                                            url: String::new(),
                                            comments,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(threads)
    }

    pub fn add_review_thread_reply(
        &self,
        input: AddReviewThreadReplyInput,
    ) -> Result<(), GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let variables = serde_json::json!({
            "threadId": input.thread_id,
            "body": input.body,
        });
        self.run_gh_graphql(
            cwd,
            crate::graphql::ADD_REVIEW_THREAD_REPLY_MUTATION,
            &[
                ("threadId", variables["threadId"].clone()),
                ("body", variables["body"].clone()),
            ],
        )?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // COMPARE / DIFF
    // -----------------------------------------------------------------------

    pub fn compare_commits(
        &self,
        input: CompareCommitsInput,
    ) -> Result<CompareCommitsResult, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let result = self.run_gh(
            cwd,
            "",
            &[
                "api",
                &format!(
                    "repos/{}/compare/{}...{}",
                    input.repo, input.base, input.head
                ),
            ],
        )?;
        let data: HashMap<String, Value> =
            decode_json_object(&result.stdout).map_err(GitHubError::JsonParse)?;
        Ok(CompareCommitsResult {
            status: data.get("status").map(as_string).unwrap_or_default(),
        })
    }

    pub fn get_pull_request_diff(
        &self,
        input: GetPullRequestDiffInput,
    ) -> Result<String, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let result = self.run_gh_with_timeout(
            cwd,
            "",
            PR_DIFF_GH_COMMAND_TIMEOUT,
            &[
                "pr",
                "diff",
                &input.pr_number.to_string(),
                "--repo",
                &input.repo,
            ],
        )?;
        // Check for diff-too-large sentinel
        if result.stdout.is_empty() && !result.stderr.is_empty() {
            return Err(GitHubError::DiffTooLarge(result.stderr));
        }
        Ok(result.stdout)
    }

    // -----------------------------------------------------------------------
    // REVIEW SUBMISSION
    // -----------------------------------------------------------------------

    pub fn submit_review(&self, input: SubmitReviewInput) -> Result<(), GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };

        // 1. Build diagnostic request map
        let mut diag_data = HashMap::new();
        diag_data.insert(
            "repo".into(),
            Value::String(input.repo.clone()),
        );
        diag_data.insert(
            "pr_number".into(),
            Value::Number(input.pr_number.into()),
        );
        diag_data.insert(
            "event".into(),
            Value::String(input.event.clone()),
        );
        diag_data.insert(
            "commit_id".into(),
            Value::String(input.commit_id.clone()),
        );
        diag_data.insert(
            "body_summary".into(),
            Value::String(review_submit_body_marker_summary(&input.body)["length"].to_string()),
        );
        diag_data.insert(
            "comments_count".into(),
            Value::Number(input.comments.len().into()),
        );
        self.emit_diagnostic("prepared", diag_data);

        // 2. Check if we have inline comments
        let has_inline_comments = !input.comments.is_empty();
        let has_commit_id = !input.commit_id.is_empty();

        if has_inline_comments || has_commit_id {
            // Use REST API with inline comments
            let request_body = review_submit_request(&input);
            let payload = serde_json::to_string(&request_body)
                .map_err(GitHubError::JsonParse)?;
            let result = self.run_gh(
                cwd,
                &payload,
                &[
                    "api",
                    &format!(
                        "repos/{}/pulls/{}/reviews",
                        input.repo, input.pr_number
                    ),
                    "--method",
                    "POST",
                    "--input",
                    "-",
                ],
            );

            match result {
                Ok(_) => {
                    self.emit_diagnostic(
                        "submitted",
                        HashMap::new(),
                    );
                    Ok(())
                }
                Err(e) => {
                    let mut err_data = HashMap::new();
                    err_data.insert(
                        "error".into(),
                        Value::String(e.to_string()),
                    );
                    self.emit_diagnostic("failed", err_data);
                    Err(e)
                }
            }
        } else {
            // Fall back to gh pr review CLI
            let event_flag = match input.event.as_str() {
                "APPROVE" => "--approve",
                "REQUEST_CHANGES" => "--request-changes",
                _ => "--comment",
            };
            let pr_str = input.pr_number.to_string();
            if !input.body.is_empty() {
                // For simple reviews, pass body via stdin
                let result = self.run_gh_with_timeout(
                    cwd,
                    &input.body,
                    DEFAULT_GH_COMMAND_TIMEOUT,
                    &[
                        "pr",
                        "review",
                        &pr_str,
                        "--repo",
                        &input.repo,
                        event_flag,
                        "--body",
                        "-",
                    ],
                );
                return match result {
                    Ok(_) => Ok(()),
                    Err(e) => {
                        let mut err_data = HashMap::new();
                        err_data.insert("error".into(), Value::String(e.to_string()));
                        self.emit_diagnostic("failed", err_data);
                        Err(e)
                    }
                };
            }
            let args: Vec<&str> = vec![
                "pr",
                "review",
                &pr_str,
                "--repo",
                &input.repo,
                event_flag,
            ];
            self.run_gh(cwd, "", &args)?;
            Ok(())
        }
    }

    // -----------------------------------------------------------------------
    // REVIEW MARKERS / COMMENTS / REACTIONS
    // -----------------------------------------------------------------------

    pub fn add_pull_request_comment(
        &self,
        input: PullRequestCommentInput,
    ) -> Result<(), GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        self.run_gh(
            cwd,
            &input.body,
            &[
                "pr",
                "comment",
                &input.pr_number.to_string(),
                "--repo",
                &input.repo,
                "--body",
                "-",
            ],
        )?;
        Ok(())
    }

    pub fn has_review_marker(
        &self,
        input: VerifyReviewMarkerInput,
    ) -> Result<bool, GitHubError> {
        let result = self.find_review_marker(input)?;
        Ok(result.found)
    }

    pub fn find_review_marker(
        &self,
        input: VerifyReviewMarkerInput,
    ) -> Result<ReviewMarkerResult, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };

        // Fetch all reviews on this PR
        let result = self.run_gh(
            cwd,
            "",
            &[
                "api",
                "--paginate",
                "--slurp",
                &format!(
                    "repos/{}/pulls/{}/reviews",
                    input.repo, input.pr_number
                ),
            ],
        )?;
        let reviews: Vec<HashMap<String, Value>> =
            decode_json_array_or_pages(&result.stdout).map_err(GitHubError::JsonParse)?;

        // If a specific author_login is given, filter by it
        let target_reviews: Vec<&HashMap<String, Value>> = if input.author_login.is_empty() {
            reviews.iter().collect()
        } else {
            reviews
                .iter()
                .filter(|r| {
                    let author = nested_string(r, &["user", "login"]);
                    normalize_github_login(&author)
                        == normalize_github_login(&input.author_login)
                })
                .collect()
        };

        for review in &target_reviews {
            let body = review.get("body").map(as_string).unwrap_or_default();
            if let Some(marker) = find_review_idempotency_marker(&body, &input.marker) {
                let event = {
                    let state = review.get("state").map(as_string).unwrap_or_default();
                    review_event_from_state(&state).to_string()
                };
                if review_event_allowed(&event, &input.allowed_review_events) {
                    let inline_comment_bodies = review
                        .get("comments")
                        .map(|c| {
                            c.as_array()
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|v| {
                                            v.as_object()
                                                .and_then(|m| m.get("body"))
                                                .map(as_string)
                                        })
                                        .collect()
                                })
                                .unwrap_or_default()
                        })
                        .unwrap_or_default();
                    return Ok(ReviewMarkerResult {
                        found: true,
                        outcome: marker.outcome.clone(),
                        event,
                        author_login: nested_string(review, &["user", "login"]),
                        body,
                        review_id: review
                            .get("id")
                            .map(as_string)
                            .unwrap_or_default(),
                        inline_comment_bodies,
                    });
                }
            }
        }

        // If allow_clean_comment, also check for plain comment reviews
        if input.allow_clean_comment {
            for review in &target_reviews {
                let body = review.get("body").map(as_string).unwrap_or_default();
                let state = review.get("state").map(as_string).unwrap_or_default();
                let event = review_event_from_state(&state);
                if review_event_allowed(event, &input.allowed_review_events)
                    && !body.contains("looper-review:")
                {
                    return Ok(ReviewMarkerResult {
                        found: true,
                        outcome: "clean".into(),
                        event: event.to_string(),
                        author_login: nested_string(review, &["user", "login"]),
                        body,
                        review_id: review
                            .get("id")
                            .map(as_string)
                            .unwrap_or_default(),
                        inline_comment_bodies: vec![],
                    });
                }
            }
        }

        Ok(ReviewMarkerResult {
            found: false,
            outcome: String::new(),
            event: String::new(),
            author_login: String::new(),
            body: String::new(),
            review_id: String::new(),
            inline_comment_bodies: vec![],
        })
    }

    pub fn add_pull_request_reaction(
        &self,
        input: PullRequestReactionInput,
    ) -> Result<(), GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        self.run_gh(
            cwd,
            "",
            &[
                "api",
                &format!("repos/{}/issues/{}/reactions", input.repo, input.pr_number),
                "--method",
                "POST",
                "-f",
                &format!("content={}", input.content),
            ],
        )?;
        Ok(())
    }

    pub fn remove_pull_request_reaction(
        &self,
        input: PullRequestReactionInput,
    ) -> Result<(), GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        // List reactions, find matching one for current user, delete it
        let result = self.run_gh(
            cwd,
            "",
            &[
                "api",
                &format!(
                    "repos/{}/issues/{}/reactions",
                    input.repo, input.pr_number
                ),
            ],
        );
        if let Ok(res) = result {
            let items: Vec<HashMap<String, Value>> =
                decode_json_array(&res.stdout).map_err(GitHubError::JsonParse)?;
            // Get current user
            if let Ok(user) = self.get_current_user_login(cwd) {
                for item in &items {
                    let item_user = nested_string(item, &["user", "login"]);
                    let item_content = item.get("content").map(as_string).unwrap_or_default();
                    if item_user == user && item_content == input.content {
                        if let Some(id) = item.get("id").map(as_i64) {
                            let _ = self.run_gh(
                                cwd,
                                "",
                                &[
                                    "api",
                                    &format!(
                                        "repos/{}/issues/{}/reactions/{}",
                                        input.repo, input.pr_number, id
                                    ),
                                    "--method",
                                    "DELETE",
                                ],
                            );
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub fn add_pull_request_labels(
        &self,
        input: PullRequestLabelsInput,
    ) -> Result<(), GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        // Ensure labels exist first
        let _ = self.ensure_labels_exist(cwd, &input.repo, &input.labels);
        let payload = serde_json::json!({"labels": input.labels}).to_string();
        self.run_gh(
            cwd,
            &payload,
            &[
                "api",
                &format!("repos/{}/issues/{}/labels", input.repo, input.pr_number),
                "--method",
                "POST",
                "--input",
                "-",
            ],
        )?;
        Ok(())
    }

    pub fn remove_pull_request_labels(
        &self,
        input: PullRequestLabelsInput,
    ) -> Result<(), GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        for label in &input.labels {
            let encoded = encode_uri_component(label);
            let _ = self.run_gh(
                cwd,
                "",
                &[
                    "api",
                    &format!(
                        "repos/{}/issues/{}/labels/{}",
                        input.repo, input.pr_number, encoded
                    ),
                    "--method",
                    "DELETE",
                ],
            );
        }
        Ok(())
    }

    pub fn add_pull_request_reviewers(
        &self,
        input: PullRequestReviewersInput,
    ) -> Result<(), GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let endpoint = format!(
            "repos/{}/pulls/{}/requested_reviewers",
            input.repo, input.pr_number
        );
        let mut flag_args: Vec<String> = Vec::new();
        for reviewer in &input.reviewers {
            flag_args.push("-f".to_string());
            flag_args.push(format!("reviewers[]={}", reviewer));
        }
        let flag_refs: Vec<&str> = flag_args.iter().map(|s| s.as_str()).collect();
        let mut args: Vec<&str> = vec!["api", &endpoint, "--method", "POST"];
        args.extend(flag_refs);
        self.run_gh(cwd, "", &args)?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // PR CREATE / UPDATE / COMPARE
    // -----------------------------------------------------------------------

    pub fn create_pull_request(
        &self,
        input: CreatePullRequestInput,
    ) -> Result<CreatePullRequestResult, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let result = self.run_gh(
            cwd,
            "",
            &[
                "pr",
                "create",
                "--repo",
                &input.repo,
                "--head",
                &input.head_branch,
                "--base",
                &input.base_branch,
                "--title",
                &input.title,
                "--body",
                &input.body,
            ],
        )?;
        let url = result.stdout.trim().to_string();
        let number = parse_pr_number_from_url(&url);
        Ok(CreatePullRequestResult { number, url })
    }

    pub fn compare_branches(
        &self,
        input: CompareBranchesInput,
    ) -> Result<CompareBranchesResult, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let result = self.run_gh(
            cwd,
            "",
            &[
                "api",
                &format!(
                    "repos/{}/compare/{}...{}",
                    input.repo, input.base_branch, input.head_branch
                ),
            ],
        )?;
        let data: HashMap<String, Value> =
            decode_json_object(&result.stdout).map_err(GitHubError::JsonParse)?;
        Ok(CompareBranchesResult {
            ahead_by: data.get("ahead_by").map(as_i64).unwrap_or(0) as i32,
            behind_by: data.get("behind_by").map(as_i64).unwrap_or(0) as i32,
            status: data.get("status").map(as_string).unwrap_or_default(),
            total_commits: data.get("total_commits").map(as_i64).unwrap_or(0) as i32,
        })
    }

    pub fn update_pull_request_title(
        &self,
        input: UpdatePullRequestTitleInput,
    ) -> Result<(), GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        self.run_gh(
            cwd,
            "",
            &[
                "pr",
                "edit",
                &input.pr_number.to_string(),
                "--repo",
                &input.repo,
                "--title",
                &input.title,
            ],
        )?;
        Ok(())
    }

    pub fn update_pull_request_body(
        &self,
        input: UpdatePullRequestBodyInput,
    ) -> Result<(), GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        self.run_gh(
            cwd,
            "",
            &[
                "pr",
                "edit",
                &input.pr_number.to_string(),
                "--repo",
                &input.repo,
                "--body",
                &input.body,
            ],
        )?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // AUTH / USER / REPO
    // -----------------------------------------------------------------------

    pub fn is_authenticated(&self, cwd: &str, hostname: &str) -> Result<bool, GitHubError> {
        let mut args = vec!["auth", "status"];
        if !hostname.is_empty() {
            args.push("--hostname");
            args.push(hostname);
        }
        match self.run_gh(cwd, "", &args) {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }

    pub fn get_current_user_login(&self, cwd: &str) -> Result<String, GitHubError> {
        let result = self.run_gh(cwd, "", &["api", "user", "--jq", ".login"]);
        match result {
            Ok(res) => Ok(res.stdout.trim().to_string()),
            Err(_) => {
                // Fallback to GraphQL
                let gql_result = self.run_gh_graphql(
                    cwd,
                    crate::graphql::VIEWER_LOGIN_QUERY,
                    &[],
                )?;
                if let Some(data) = gql_result.as_object() {
                    if let Some(viewer) = data.get("viewer") {
                        return Ok(viewer
                            .as_object()
                            .and_then(|v| v.get("login"))
                            .map(as_string)
                            .unwrap_or_default());
                    }
                }
                Err(GitHubError::Auth(
                    "could not determine current user".into(),
                ))
            }
        }
    }

    pub fn get_current_user_identity(
        &self,
        cwd: &str,
    ) -> Result<CurrentUserIdentity, GitHubError> {
        let result = self.run_gh(
            cwd,
            "",
            &[
                "api",
                "user",
                "--jq",
                "{login: .login, id: .id}",
            ],
        )?;
        let data: HashMap<String, Value> =
            decode_json_object(&result.stdout).map_err(GitHubError::JsonParse)?;
        Ok(CurrentUserIdentity {
            login: data.get("login").map(as_string).unwrap_or_default(),
            numeric_id: data.get("id").map(as_i64).unwrap_or(0),
        })
    }

    pub fn get_current_user_login_for_repo(
        &self,
        _repo: &str,
        cwd: &str,
    ) -> Result<String, GitHubError> {
        self.get_current_user_login(cwd)
    }

    pub fn detect_current_repository(&self, cwd: &str) -> Result<String, GitHubError> {
        let result = self.run_gh(
            cwd,
            "",
            &["repo", "view", "--json", "nameWithOwner,url"],
        )?;
        let data: HashMap<String, Value> =
            decode_json_object(&result.stdout).map_err(GitHubError::JsonParse)?;
        Ok(data
            .get("nameWithOwner")
            .map(as_string)
            .unwrap_or_default())
    }

    // -----------------------------------------------------------------------
    // ISSUE DEPENDENCY HELPERS
    // -----------------------------------------------------------------------

    /// List dependencies where this issue is blocked by.
    pub fn list_blocked_by_issues(
        &self,
        input: ViewIssueInput,
    ) -> Result<Vec<DependencyIssue>, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        self.list_dependency_issues(cwd, &input.repo, input.issue_number, "blocked_by")
    }

    /// List issues that this issue is blocking.
    pub fn list_blocking_issues(
        &self,
        input: ViewIssueInput,
    ) -> Result<Vec<DependencyIssue>, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        self.list_dependency_issues(cwd, &input.repo, input.issue_number, "blocking")
    }

    /// List sub-issues of this issue.
    pub fn list_sub_issues(
        &self,
        input: ViewIssueInput,
    ) -> Result<Vec<DependencyIssue>, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        self.list_dependency_issues(cwd, &input.repo, input.issue_number, "sub_issues")
    }

    fn list_dependency_issues(
        &self,
        cwd: &str,
        repo: &str,
        issue_number: i64,
        dependency_type: &str,
    ) -> Result<Vec<DependencyIssue>, GitHubError> {
        let result = self.run_gh(
            cwd,
            "",
            &[
                "api",
                "--paginate",
                "--slurp",
                &format!(
                    "repos/{}/issues/{}/dependencies/{}",
                    repo, issue_number, dependency_type
                ),
            ],
        )?;
        let items: Vec<HashMap<String, Value>> =
            decode_json_array_or_pages(&result.stdout).map_err(GitHubError::JsonParse)?;
        Ok(items
            .into_iter()
            .map(|m| extract_dependency_issue(&m, repo))
            .collect())
    }

    /// Find any issue number in a repository (skips PRs).
    pub fn find_any_issue_number(&self, repo: &str, cwd: &str) -> Result<i64, GitHubError> {
        let result = self.run_gh(
            cwd,
            "",
            &[
                "issue",
                "list",
                "--repo",
                repo,
                "--state",
                "open",
                "--limit",
                "30",
                "--json",
                "number,isPullRequest",
            ],
        )?;
        let items: Vec<HashMap<String, Value>> =
            decode_json_array(&result.stdout).map_err(GitHubError::JsonParse)?;
        for item in &items {
            let is_pr = item
                .get("isPullRequest")
                .map(as_bool)
                .unwrap_or(false);
            if !is_pr {
                return Ok(item.get("number").map(as_i64).unwrap_or(0));
            }
        }
        Err(GitHubError::Empty(
            "no issues found in repository".into(),
        ))
    }

    // -----------------------------------------------------------------------
    // LABEL INITIALIZATION
    // -----------------------------------------------------------------------

    pub fn initialize_labels(
        &self,
        input: InitializeLabelsInput,
    ) -> Result<LabelInitResult, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        let standard_labels = crate::types::standard_looper_labels();
        let mut items = Vec::new();
        let mut summary = LabelInitSummary {
            created: 0,
            updated: 0,
            skipped: 0,
            failed: 0,
        };

        // Get existing labels
        let existing = self.list_repository_labels(cwd, &input.repo).ok();

        for label_def in &standard_labels {
            let mut error = String::new();

            let should_create = if let Some(ref existing_labels) = existing {
                !existing_labels.contains_key(&label_def.name)
            } else {
                true
            };

            let status = if !input.dry_run {
                if should_create {
                    match self.run_gh(
                        cwd,
                        "",
                        &[
                            "label",
                            "create",
                            &label_def.name,
                            "--repo",
                            &input.repo,
                            "--color",
                            &label_def.color,
                            "--description",
                            &label_def.description,
                            "--force",
                        ],
                    ) {
                        Ok(_) => "created",
                        Err(e) => {
                            error = e.to_string();
                            "failed"
                        }
                    }
                } else {
                    match self.run_gh(
                        cwd,
                        "",
                        &[
                            "label",
                            "edit",
                            &label_def.name,
                            "--repo",
                            &input.repo,
                            "--color",
                            &label_def.color,
                            "--description",
                            &label_def.description,
                        ],
                    ) {
                        Ok(_) => "updated",
                        Err(e) => {
                            error = e.to_string();
                            "failed"
                        }
                    }
                }
            } else if should_create {
                "created"
            } else {
                "updated"
            };

            increment_label_summary(&mut summary, status);
            items.push(LabelInitItem {
                status: status.to_string(),
                name: label_def.name.clone(),
                color: label_def.color.clone(),
                description: label_def.description.clone(),
                error: if error.is_empty() {
                    None
                } else {
                    Some(error)
                },
            });
        }

        Ok(LabelInitResult {
            repo: input.repo,
            dry_run: input.dry_run,
            labels: items,
            summary,
        })
    }

    // -----------------------------------------------------------------------
    // SNAPSHOT
    // -----------------------------------------------------------------------

    pub fn capture_pull_request_snapshot(
        &self,
        input: CapturePullRequestSnapshotInput,
    ) -> Result<PullRequestSnapshotRecord, GitHubError> {
        let cwd = if input.cwd.is_empty() {
            &self.cwd
        } else {
            &input.cwd
        };
        // View PR detail
        let pr = self.view_pull_request(ViewPullRequestInput {
            repo: input.repo.clone(),
            pr_number: input.pr_number,
            cwd: cwd.to_string(),
        })?;

        // Get diff
        let diff_result = self.get_pull_request_diff(GetPullRequestDiffInput {
            repo: input.repo.clone(),
            pr_number: input.pr_number,
            cwd: cwd.to_string(),
        });
        let diff = diff_result.unwrap_or_default();

        // Build snapshot record
        Ok(PullRequestSnapshotRecord {
            project_id: input.project_id,
            repo: input.repo,
            pr_number: input.pr_number,
            pr_title: pr.title,
            pr_body: pr.body,
            pr_head_sha: pr.head_sha,
            pr_base_sha: pr.base_sha,
            diff,
            captured_at: input.captured_at,
        })
    }

    // -----------------------------------------------------------------------
    // INTERNAL HELPERS
    // -----------------------------------------------------------------------

    fn run_gh_graphql(
        &self,
        cwd: &str,
        query: &str,
        variables: &[(&str, Value)],
    ) -> Result<Value, GitHubError> {
        let query_arg = format!("query={}", query);
        // Build variable args as Strings first to avoid borrow issues
        let mut var_strings: Vec<String> = Vec::new();
        for (name, value) in variables {
            let serialized = serde_json::to_string(value).map_err(GitHubError::JsonParse)?;
            var_strings.push("-F".to_string());
            var_strings.push(format!("{}={}", name, serialized));
        }
        let var_refs: Vec<&str> = var_strings.iter().map(|s| s.as_str()).collect();
        let mut args: Vec<&str> = vec!["api", "graphql", "-f", &query_arg];
        args.extend(var_refs);
        let result = self.run_gh(cwd, "", &args)?;
        let data: Value =
            serde_json::from_str(&result.stdout).map_err(GitHubError::JsonParse)?;
        // Extract data from {"data": {...}} response
        if let Some(obj) = data.as_object() {
            if let Some(data_obj) = obj.get("data") {
                return Ok(data_obj.clone());
            }
            if let Some(errors) = obj.get("errors") {
                let msg = errors
                    .as_array()
                    .and_then(|arr| arr.first())
                    .and_then(|e| e.as_object())
                    .and_then(|e| e.get("message"))
                    .map(as_string)
                    .unwrap_or_else(|| "GraphQL error".into());
                return Err(GitHubError::Api(msg, 422));
            }
        }
        Ok(data)
    }

    fn ensure_labels_exist(
        &self,
        cwd: &str,
        repo: &str,
        labels: &[String],
    ) -> Result<(), GitHubError> {
        let existing = self.list_repository_labels(cwd, repo).ok();
        for label in labels {
            let should_create = if let Some(ref existing_labels) = existing {
                !existing_labels.contains_key(label)
            } else {
                true
            };
            if should_create {
                let color = resolve_label_color(label);
                let description = resolve_label_description(label);
                let _ = self.run_gh(
                    cwd,
                    "",
                    &[
                        "label",
                        "create",
                        label,
                        "--repo",
                        repo,
                        "--color",
                        color,
                        "--description",
                        description,
                        "--force",
                    ],
                );
            }
        }
        Ok(())
    }

    fn list_repository_labels(
        &self,
        cwd: &str,
        repo: &str,
    ) -> Result<HashMap<String, LabelDefinition>, GitHubError> {
        let result = self.run_gh(
            cwd,
            "",
            &[
                "label",
                "list",
                "--repo",
                repo,
                "--limit",
                "1000",
                "--json",
                "name,color,description",
            ],
        )?;
        let items: Vec<HashMap<String, Value>> =
            decode_json_array(&result.stdout).map_err(GitHubError::JsonParse)?;
        let mut labels = HashMap::new();
        for item in &items {
            let name = item.get("name").map(as_string).unwrap_or_default();
            if !name.is_empty() {
                labels.insert(
                    name.clone(),
                    LabelDefinition {
                        name,
                        color: item.get("color").map(as_string).unwrap_or_default(),
                        description: item
                            .get("description")
                            .map(as_string)
                            .unwrap_or_default(),
                    },
                );
            }
        }
        Ok(labels)
    }
}

// ---------------------------------------------------------------------------
// Default gh command runner
// ---------------------------------------------------------------------------

#[allow(clippy::disallowed_methods)]
fn run_gh_command(gh_path: &str, opts: &ShellOptions) -> Result<ShellResult, GitHubError> {
    let mut cmd = std::process::Command::new(gh_path);
    cmd.args(&opts.args);
    cmd.current_dir(&opts.cwd);

    if opts.stdin.is_some() {
        cmd.stdin(std::process::Stdio::piped());
    }
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| {
        GitHubError::CommandExecution(format!("failed to spawn gh: {}", e))
    })?;

    if let Some(ref stdin_data) = opts.stdin {
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(stdin_data.as_bytes());
        }
    }

    let output = child.wait_with_output().map_err(|e| {
        GitHubError::CommandExecution(format!("failed to wait for gh: {}", e))
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code().unwrap_or(-1);

    if !output.status.success() {
        // Check for common transient patterns
        let combined = format!("{} {}", stdout, stderr).to_lowercase();
        if crate::error::TRANSIENT_PATTERNS
            .iter()
            .any(|p| combined.contains(p))
        {
            return Err(GitHubError::Transient(TransientError::new(
                format!("gh command failed: {} (exit: {})", stderr.trim(), exit_code),
            )));
        }
        return Err(GitHubError::CommandFailed(format!(
            "gh command failed: {} (exit: {})",
            stderr.trim(),
            exit_code
        )));
    }

    Ok(ShellResult {
        stdout,
        stderr,
        exit_code,
    })
}

// ---------------------------------------------------------------------------
// PR summary parser
// ---------------------------------------------------------------------------

fn parse_pr_summary(data: HashMap<String, Value>) -> PullRequestSummary {
    let has_conflicts = data
        .get("mergeStateStatus")
        .map(as_string)
        .unwrap_or_default()
        == "DIRTY";

    PullRequestSummary {
        number: data.get("number").map(as_i64).unwrap_or(0),
        title: data.get("title").map(as_string).unwrap_or_default(),
        url: data.get("url").map(as_string).unwrap_or_default(),
        state: data.get("state").map(as_string).unwrap_or_default(),
        updated_at: data.get("updatedAt").map(as_string).unwrap_or_default(),
        is_draft: data.get("isDraft").map(as_bool).unwrap_or(false),
        review_decision: data
            .get("reviewDecision")
            .map(as_string)
            .unwrap_or_default(),
        labels: data.get("labels").map(extract_label_names).unwrap_or_default(),
        head_ref_name: data
            .get("headRefName")
            .map(as_string)
            .unwrap_or_default(),
        base_ref_name: data
            .get("baseRefName")
            .map(as_string)
            .unwrap_or_default(),
        head_sha: data
            .get("headRefOid")
            .map(as_string)
            .unwrap_or_default(),
        base_sha: data
            .get("baseRefOid")
            .map(as_string)
            .unwrap_or_default(),
        has_conflicts,
        author: extract_author(data.get("author").unwrap_or(&Value::Null)),
        author_association: String::new(),
        review_requests: data
            .get("reviewRequests")
            .map(extract_review_request_logins)
            .unwrap_or_default(),
        review_request_users: data
            .get("reviewRequests")
            .map(extract_review_request_users)
            .unwrap_or_default(),
        reviews: to_object_slice(data.get("reviews").unwrap_or(&Value::Null)),
    }
}

// ---------------------------------------------------------------------------
// PR detail parser
// ---------------------------------------------------------------------------

fn parse_pr_detail(data: &HashMap<String, Value>) -> Result<PullRequestDetail, GitHubError> {
    let has_conflicts = data
        .get("mergeStateStatus")
        .map(as_string)
        .unwrap_or_default()
        == "DIRTY";

    let issue_comments: Vec<HashMap<String, Value>> = data
        .get("comments")
        .map(to_object_slice)
        .unwrap_or_default();
    let comment_infos: Vec<CommentInfo> = issue_comments
        .iter()
        .map(parse_comment_info)
        .collect();

    let checks: Vec<HashMap<String, Value>> = data
        .get("statusCheckRollup")
        .map(to_object_slice)
        .unwrap_or_default();

    Ok(PullRequestDetail {
        number: data.get("number").map(as_i64).unwrap_or(0),
        title: data.get("title").map(as_string).unwrap_or_default(),
        body: data.get("body").map(as_string).unwrap_or_default(),
        url: data.get("url").map(as_string).unwrap_or_default(),
        state: data.get("state").map(as_string).unwrap_or_default(),
        created_at: data.get("createdAt").map(as_string).unwrap_or_default(),
        updated_at: data.get("updatedAt").map(as_string).unwrap_or_default(),
        closed_at: data.get("closedAt").map(as_string).unwrap_or_default(),
        is_draft: data.get("isDraft").map(as_bool).unwrap_or(false),
        review_decision: data
            .get("reviewDecision")
            .map(as_string)
            .unwrap_or_default(),
        labels: data.get("labels").map(extract_label_names).unwrap_or_default(),
        head_ref_name: data
            .get("headRefName")
            .map(as_string)
            .unwrap_or_default(),
        base_ref_name: data
            .get("baseRefName")
            .map(as_string)
            .unwrap_or_default(),
        head_sha: data.get("headRefOid").map(as_string).unwrap_or_default(),
        base_sha: data.get("baseRefOid").map(as_string).unwrap_or_default(),
        author: extract_author(data.get("author").unwrap_or(&Value::Null)),
        author_association: String::new(),
        comment_count: comment_infos.len() as i32,
        review_requests: data
            .get("reviewRequests")
            .map(extract_review_request_logins)
            .unwrap_or_default(),
        review_request_users: data
            .get("reviewRequests")
            .map(extract_review_request_users)
            .unwrap_or_default(),
        has_conflicts,
        comments: to_object_slice(data.get("comments").unwrap_or(&Value::Null)),
        issue_comments: comment_infos,
        reviews: to_object_slice(data.get("reviews").unwrap_or(&Value::Null)),
        checks,
        mergeable: None,
        mergeable_state: String::new(),
        merged_at: data.get("mergedAt").map(as_string).unwrap_or_default(),
        auto_merge: data
            .get("autoMerge")
            .and_then(extract_auto_merge),
    })
}

// ---------------------------------------------------------------------------
// Issue summary parser
// ---------------------------------------------------------------------------

fn parse_issue_summary(data: HashMap<String, Value>) -> IssueSummary {
    IssueSummary {
        number: data.get("number").map(as_i64).unwrap_or(0),
        title: data.get("title").map(as_string).unwrap_or_default(),
        body: data.get("body").map(as_string).unwrap_or_default(),
        url: data.get("url").map(as_string).unwrap_or_default(),
        state: data.get("state").map(as_string).unwrap_or_default(),
        updated_at: data.get("updatedAt").map(as_string).unwrap_or_default(),
        author: extract_author(data.get("author").unwrap_or(&Value::Null)),
        author_association: String::new(),
        assignees: data
            .get("assignees")
            .map(extract_actor_logins)
            .unwrap_or_default(),
        assignee_users: data
            .get("assignees")
            .map(extract_actor_users)
            .unwrap_or_default(),
        labels: data.get("labels").map(extract_label_names).unwrap_or_default(),
        is_pull_request: data
            .get("isPullRequest")
            .map(as_bool)
            .unwrap_or(false),
    }
}

// ---------------------------------------------------------------------------
// Issue detail parser
// ---------------------------------------------------------------------------

fn parse_issue_detail(
    data: &HashMap<String, Value>,
    comments: Vec<CommentInfo>,
) -> IssueDetail {
    IssueDetail {
        number: data.get("number").map(as_i64).unwrap_or(0),
        title: data.get("title").map(as_string).unwrap_or_default(),
        body: data.get("body").map(as_string).unwrap_or_default(),
        url: data.get("html_url").map(as_string).unwrap_or_default(),
        state: data.get("state").map(as_string).unwrap_or_default(),
        state_reason: data
            .get("state_reason")
            .map(as_string)
            .unwrap_or_default(),
        created_at: data.get("created_at").map(as_string).unwrap_or_default(),
        updated_at: data.get("updated_at").map(as_string).unwrap_or_default(),
        closed_at: data.get("closed_at").map(as_string).unwrap_or_default(),
        author: nested_string(data, &["user", "login"]),
        author_association: data
            .get("author_association")
            .map(as_string)
            .unwrap_or_default(),
        assignees: data
            .get("assignees")
            .map(to_object_slice)
            .unwrap_or_default()
            .iter()
            .map(|m| m.get("login").map(as_string).unwrap_or_default())
            .collect(),
        assignee_users: data
            .get("assignees")
            .map(to_object_slice)
            .unwrap_or_default()
            .iter()
            .map(|m| GitHubUser {
                login: m.get("login").map(as_string).unwrap_or_default(),
                id: m.get("id").map(as_i64).unwrap_or(0),
            })
            .collect(),
        labels: data
            .get("labels")
            .map(extract_label_names)
            .unwrap_or_default(),
        is_pull_request: data
            .get("pull_request")
            .map(|v| !v.is_null())
            .unwrap_or(false),
        comment_count: comments.len() as i32,
        comments,
    }
}

// ---------------------------------------------------------------------------
// Comment info parser
// ---------------------------------------------------------------------------

fn parse_comment_info(data: &HashMap<String, Value>) -> CommentInfo {
    CommentInfo {
        id: data.get("id").map(as_i64).unwrap_or(0),
        author: nested_string(data, &["user", "login"]),
        author_association: data
            .get("author_association")
            .map(as_string)
            .unwrap_or_default(),
        body: data.get("body").map(as_string).unwrap_or_default(),
        created_at: data.get("created_at").map(as_string).unwrap_or_default(),
        updated_at: data.get("updated_at").map(as_string).unwrap_or_default(),
        url: data.get("html_url").map(as_string).unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// GraphQL search node parser
// ---------------------------------------------------------------------------

fn parse_search_pr_nodes(result: &Value) -> Result<Vec<HashMap<String, Value>>, GitHubError> {
    let mut results = Vec::new();
    if let Some(data) = result.as_object() {
        if let Some(search) = data.get("search") {
            if let Some(nodes) = search.as_object().and_then(|s| s.get("nodes")) {
                if let Some(arr) = nodes.as_array() {
                    for node in arr {
                        if let Some(obj) = node.as_object() {
                            let mut map = HashMap::new();
                            map.insert(
                                "number".into(),
                                Value::Number(
                                    obj.get("number")
                                        .and_then(|v| v.as_i64())
                                        .unwrap_or(0).into(),
                                ),
                            );
                            map.insert(
                                "title".into(),
                                Value::String(
                                    obj.get("title")
                                        .map(as_string)
                                        .unwrap_or_default(),
                                ),
                            );
                            map.insert(
                                "url".into(),
                                Value::String(
                                    obj.get("url").map(as_string).unwrap_or_default(),
                                ),
                            );
                            map.insert(
                                "state".into(),
                                Value::String(
                                    obj.get("state").map(as_string).unwrap_or_default(),
                                ),
                            );
                            map.insert(
                                "updatedAt".into(),
                                Value::String(
                                    obj.get("updatedAt")
                                        .map(as_string)
                                        .unwrap_or_default(),
                                ),
                            );
                            map.insert(
                                "isDraft".into(),
                                Value::Bool(
                                    obj.get("isDraft").map(as_bool).unwrap_or(false),
                                ),
                            );
                            map.insert(
                                "reviewDecision".into(),
                                Value::String(
                                    obj.get("reviewDecision")
                                        .map(as_string)
                                        .unwrap_or_default(),
                                ),
                            );
                            map.insert(
                                "labels".into(),
                                obj.get("labels").cloned().unwrap_or(Value::Null),
                            );
                            map.insert(
                                "headRefName".into(),
                                Value::String(
                                    obj.get("headRefName")
                                        .map(as_string)
                                        .unwrap_or_default(),
                                ),
                            );
                            map.insert(
                                "baseRefName".into(),
                                Value::String(
                                    obj.get("baseRefName")
                                        .map(as_string)
                                        .unwrap_or_default(),
                                ),
                            );
                            map.insert(
                                "headRefOid".into(),
                                Value::String(
                                    obj.get("headRefOid")
                                        .map(as_string)
                                        .unwrap_or_default(),
                                ),
                            );
                            map.insert(
                                "baseRefOid".into(),
                                Value::String(
                                    obj.get("baseRefOid")
                                        .map(as_string)
                                        .unwrap_or_default(),
                                ),
                            );
                            map.insert(
                                "mergeStateStatus".into(),
                                Value::String(
                                    obj.get("mergeStateStatus")
                                        .map(as_string)
                                        .unwrap_or_default(),
                                ),
                            );
                            map.insert(
                                "author".into(),
                                obj.get("author").cloned().unwrap_or(Value::Null),
                            );
                            results.push(map);
                        }
                    }
                }
            }
        }
    }
    Ok(results)
}
