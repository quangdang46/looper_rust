//! fake-gh binary -- a mock GitHub CLI for E2E testing.
//!
//! Reads a state file (JSON) from `LOOPER_E2E_FAKE_GH_STATE_PATH` that defines
//! all commands/responses, and responds accordingly.
//!
//! Environment:
//!   LOOPER_E2E_FAKE_GH_MODE        — "strict" (default) | "record" | "replay"
//!   LOOPER_E2E_FAKE_GH_STATE_PATH   — path to state JSON
//!   LOOPER_E2E_FAKE_GH_ARTIFACT_DIR — dir for recording evidence
//!   LOOPER_E2E_FAKE_GH_SCHEMA_PATH  — path to schema JSON (field allowlist)
//!   LOOPER_E2E_FAKE_GH_RECORD_PATH  — path to record JSONL
//!   LOOPER_E2E_FAKE_GH_GIT_PATH     — path to git binary

// Suppress workspace-level unused-crate-dependencies lint: these deps are declared
// in the looper-e2e package Cargo.toml and consumed by other targets within the
// same package (the lib crate, other binary targets).
#[allow(unused_imports)]
use {
    anyhow as _, looper_agent as _, looper_config as _, looper_e2e as _, looper_git as _, looper_github as _,
    looper_runner as _, looper_scheduler as _, looper_service as _, looper_storage as _, looper_types as _,
    tempfile as _, thiserror as _, tokio as _, uuid as _,
};

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::io::BufWriter;
use std::io::IsTerminal;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::process::{self, Command};
use std::sync::LazyLock;

// ---------------------------------------------------------------------------
// Environment variable names
// ---------------------------------------------------------------------------

const ENV_MODE: &str = "LOOPER_E2E_FAKE_GH_MODE";
const ENV_ARTIFACT_DIR: &str = "LOOPER_E2E_FAKE_GH_ARTIFACT_DIR";
const ENV_SCHEMA_PATH: &str = "LOOPER_E2E_FAKE_GH_SCHEMA_PATH";
const ENV_STATE_PATH: &str = "LOOPER_E2E_FAKE_GH_STATE_PATH";
const ENV_RECORD_PATH: &str = "LOOPER_E2E_FAKE_GH_RECORD_PATH";
const ENV_GIT_PATH: &str = "LOOPER_E2E_FAKE_GH_GIT_PATH";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A single command response from the state file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Response {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stdout: Option<serde_json::Value>,
    #[serde(default)]
    stderr: String,
    #[serde(default)]
    exit_code: i32,
}

/// JSON-schema that defines which JSON fields are allowed for each command key.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Schema {
    #[serde(rename = "jsonFieldAllowlist")]
    json_field_allowlist: HashMap<String, Vec<String>>,
}

/// A review comment on a thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewCommentState {
    id: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    updated_at: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    line: i64,
    #[serde(default)]
    original_commit_oid: String,
    #[serde(default)]
    commit_oid: String,
    #[serde(default)]
    url: String,
}

/// A review thread (group of comments on the same line).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewThreadState {
    id: String,
    #[serde(default)]
    is_resolved: bool,
    #[serde(default)]
    path: String,
    #[serde(default)]
    line: i64,
    #[serde(default)]
    comments: Vec<ReviewCommentState>,
}

/// A pull request as represented in the fake-gh state.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullRequestState {
    #[serde(default)]
    number: i64,
    #[serde(default)]
    repo: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    updated_at: String,
    #[serde(default)]
    closed_at: String,
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    review_decision: String,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    head_ref_name: String,
    #[serde(default)]
    base_ref_name: String,
    #[serde(default)]
    head_ref: String,
    #[serde(default)]
    base_ref: String,
    #[serde(default)]
    head_sha: String,
    #[serde(default)]
    base_sha: String,
    #[serde(default)]
    git_dir: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    author_association: String,
    #[serde(default)]
    review_requests: Vec<String>,
    #[serde(default)]
    issue_comments: Vec<serde_json::Value>,
    #[serde(default)]
    reviews: Vec<serde_json::Value>,
    #[serde(default)]
    status_check_rollup: Vec<serde_json::Value>,
    #[serde(default)]
    merge_state_status: String,
    #[serde(default)]
    mergeable: Option<bool>,
    #[serde(default)]
    mergeable_state: String,
    #[serde(default)]
    merged_at: String,
    #[serde(default)]
    auto_merge: Option<serde_json::Value>,
    #[serde(default)]
    check_runs: Vec<serde_json::Value>,
    #[serde(default)]
    threads: Vec<ReviewThreadState>,
}

/// Complete state of the fake-gh server, serialised to JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct State {
    #[serde(default)]
    commands: HashMap<String, Response>,
    #[serde(default)]
    routes: HashMap<String, serde_json::Value>,
    #[serde(default)]
    graphql: HashMap<String, serde_json::Value>,
    #[serde(default)]
    current_user_login: String,
    #[serde(default)]
    pull_requests: HashMap<String, PullRequestState>,
}

/// An invocation record appended to the invocations JSONL.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Invocation {
    timestamp: String,
    cwd: String,
    argv: Vec<String>,
    #[serde(default)]
    stdin: String,
    env: HashMap<String, String>,
    mode: String,
}

// Lazy regex for closing issue keywords in PR bodies.
static CLOSES_ISSUE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)\s+#(\d+)\b").expect("valid closes-issue regex")
});

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let mode = env_var(ENV_MODE).unwrap_or_else(|| "strict".to_string());
    let artifact_dir = env_var(ENV_ARTIFACT_DIR).unwrap_or_else(|| ".".to_string());
    let schema_path = env_var(ENV_SCHEMA_PATH).unwrap_or_default();
    let state_path = env_var(ENV_STATE_PATH).unwrap_or_default();
    let record_path = env_var(ENV_RECORD_PATH).unwrap_or_default();
    let git_path = env_var(ENV_GIT_PATH).unwrap_or_else(|| "git".to_string());

    // Ensure artifact directory exists.
    let _ = std::fs::create_dir_all(&artifact_dir);

    // Read stdin (only if piped, not if running interactively).
    let stdin_buf: Vec<u8> = if std::io::stdin().is_terminal() {
        Vec::new()
    } else {
        let mut buf = Vec::new();
        let _ = std::io::stdin().read_to_end(&mut buf);
        buf
    };

    let stdin_str = String::from_utf8_lossy(&stdin_buf).to_string();

    // Record invocation.
    let args: Vec<String> = env::args().skip(1).collect();
    {
        let inv_path = Path::new(&artifact_dir).join("invocations.jsonl");
        let inv = Invocation {
            timestamp: iso_timestamp(),
            cwd: must_getwd(),
            argv: args.clone(),
            stdin: stdin_str.clone(),
            env: collect_env(),
            mode: mode.clone(),
        };
        let _ = append_jsonl(&inv_path, &inv);
    }

    // Load schema.
    let schema_doc = load_schema(&schema_path).unwrap_or_else(|e| {
        if mode == "strict" {
            fatalf(2, &format!("load fake-gh schema: {e}\n"));
        }
        Schema { json_field_allowlist: HashMap::new() }
    });

    // Load state.
    let st = load_state(&state_path).unwrap_or_else(|e| {
        fatalf(2, &format!("load fake-gh state: {e}\n"));
    });

    // In record mode, append to the record JSONL.
    if mode == "record" {
        let _ = append_jsonl(
            &record_path,
            &serde_json::json!({
                "argv": args,
                "stdin": stdin_str,
            }),
        );
    }

    if let Err(e) = dispatch(&mode, &schema_doc, &st, &stdin_str, &args, &state_path, &git_path) {
        fatalf(1, &format!("{e}\n"));
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

fn dispatch(
    mode: &str,
    schema_doc: &Schema,
    st: &State,
    stdin: &str,
    args: &[String],
    state_path: &str,
    git_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let key = command_key(args);
    // Pre-declared commands take priority.
    if let Some(resp) = st.commands.get(&key) {
        return emit_response(resp);
    }

    if key == "api" || key.starts_with("api ") {
        return handle_api(mode, st, stdin, args, state_path, git_path);
    }

    match key.as_str() {
        "auth status" => {
            let login = first_non_empty(&[&st.current_user_login, "looper"]);
            println!("github.com\n  ✓ Logged in to github.com as {login}");
            Ok(())
        }
        "auth token" => {
            println!("gho_fake-token-for-testing");
            Ok(())
        }
        "pr view" => {
            let fields = requested_json_fields(args);
            let allowed = schema_doc.json_field_allowlist.get(&key).cloned().unwrap_or_default();
            if allowed.is_empty() && mode == "strict" {
                return Err(format!("missing fake-gh allowlist for {key}").into());
            }
            validate_fields(&key, &fields, &allowed)?;
            if let Some(payload) = build_pr_view_json(st, args, &fields, git_path) {
                println!("{payload}");
                return Ok(());
            }
            emit_default_json(&key, &fields)
        }
        "pr merge" => {
            handle_pr_merge(st, args, state_path, git_path)?;
            println!("{{}}");
            Ok(())
        }
        "issue list" | "pr list" => {
            let fields = requested_json_fields(args);
            let allowed = schema_doc.json_field_allowlist.get(&key).cloned().unwrap_or_default();
            if allowed.is_empty() && mode == "strict" {
                return Err(format!("missing fake-gh allowlist for {key}").into());
            }
            validate_fields(&key, &fields, &allowed)?;
            emit_default_json(&key, &fields)
        }
        _ => {
            if mode == "strict" {
                return Err(format!("unsupported fake-gh command: {}", args.join(" ")).into());
            }
            println!("{{}}");
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Command handlers
// ---------------------------------------------------------------------------

fn handle_api(
    mode: &str,
    st: &State,
    stdin: &str,
    args: &[String],
    state_path: &str,
    git_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let route = first_non_flag(&args[1..]);

    // "api user"
    if route == "user" {
        let login = first_non_empty(&[&st.current_user_login, "looper"]);
        if has_arg(args, "--jq", ".login") {
            println!("{login}");
            return Ok(());
        }
        println!("{{\"login\":\"{login}\"}}");
        return Ok(());
    }

    // "api graphql ..."
    if args.len() >= 2 && args[1] == "graphql" {
        if let Some(true) = handle_graphql_state(st, args, stdin, state_path, git_path)? {
            return Ok(());
        }
        let operation = graphql_operation(args, stdin);
        if let Some(payload) = st.graphql.get(&operation) {
            emit_json_value(payload);
            return Ok(());
        }
        println!("{{\"data\":{{}}}}");
        return Ok(());
    }

    // Reviews
    if route.contains("/pulls/") && route.ends_with("/reviews") {
        if let Some(true) = handle_pr_reviews(st, args, stdin, &route, state_path)? {
            return Ok(());
        }
    }

    // Review comments
    if route.contains("/pulls/") && route.contains("/reviews/") && route.ends_with("/comments") {
        if let Some(true) = handle_pr_review_comments(st, &route)? {
            return Ok(());
        }
    }

    // PR API
    if let Some(true) = handle_pr_api(st, &route, git_path)? {
        return Ok(());
    }

    // Check-runs API
    if let Some(true) = handle_check_runs_api(st, &route)? {
        return Ok(());
    }

    // POST comments
    if route.ends_with("/comments") && flag_value(args, "--method").to_uppercase() == "POST" {
        println!("{{\"id\":1,\"html_url\":\"https://example.test/issues/comments/1\"}}");
        return Ok(());
    }

    // Routes map
    if let Some(payload) = st.routes.get(&route) {
        emit_json_value(payload);
        return Ok(());
    }

    // Paginate
    if args.contains(&"--paginate".to_string()) {
        println!("[]");
        return Ok(());
    }

    // Compare
    if route.contains("/compare/") {
        if let Some(payload) = build_compare_payload(&route, git_path) {
            println!("{payload}");
            return Ok(());
        }
    }

    if mode == "strict" && route.is_empty() {
        return Err(format!("unsupported fake-gh api invocation: {}", args.join(" ")).into());
    }

    println!("{{\"id\":1,\"number\":1,\"title\":\"fake issue\"}}");
    let _ = stdin;
    Ok(())
}

/// Build a `pr view` JSON from the state's PullRequests.
fn build_pr_view_json(st: &State, args: &[String], fields: &[String], git_path: &str) -> Option<String> {
    let repo = flag_value(args, "--repo");
    let pr_number = parse_pr_number(args)?;
    let pr = lookup_pull_request(st, &repo, pr_number, git_path)?;

    let mut row = serde_json::Map::new();
    for field in fields {
        row.insert(field.clone(), pr_field_value(&pr, field));
    }
    serde_json::to_string(&row).ok()
}

/// Merge a pull request (with optional auto-merge or linked-issue closing).
fn handle_pr_merge(
    _original_st: &State,
    args: &[String],
    state_path: &str,
    _git_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let pr_number_str = first_non_flag(&args[2..]);
    if pr_number_str.is_empty() {
        return Ok(());
    }
    let pr_number: i64 = match pr_number_str.parse() {
        Ok(n) => n,
        Err(_) => return Ok(()),
    };
    let repo = flag_value(args, "--repo");
    if repo.is_empty() {
        return Ok(());
    }
    let key = format!("{repo}#{pr_number}");

    let mut st = load_state(state_path)?;
    let pr = match st.pull_requests.get_mut(&key) {
        Some(p) => p,
        None => return Ok(()),
    };

    if args.contains(&"--auto".to_string()) {
        pr.auto_merge = Some(serde_json::json!({
            "enabledBy": {"login": first_non_empty(&[&st.current_user_login, "looper"])},
            "mergeMethod": pr_merge_method(args)
        }));
        if pr.updated_at.is_empty() {
            pr.updated_at = "2026-05-12T00:00:00Z".to_string();
        }
    } else {
        let body = pr.body.clone();
        pr.state = "MERGED".to_string();
        if pr.closed_at.is_empty() {
            pr.closed_at = "2026-05-12T00:00:00Z".to_string();
        }
        if pr.merged_at.is_empty() {
            pr.merged_at = pr.closed_at.clone();
        }
        if pr.updated_at.is_empty() {
            pr.updated_at = "2026-05-12T00:00:00Z".to_string();
        }

        // Close linked issues
        for cap in CLOSES_ISSUE_RE.captures_iter(&body) {
            if let Some(m) = cap.get(1) {
                if let Ok(issue_num) = m.as_str().parse::<i64>() {
                    close_linked_issue_route(&mut st, &repo, issue_num);
                }
            }
        }
    }

    save_state(state_path, &st)
}

fn close_linked_issue_route(st: &mut State, repo: &str, issue_number: i64) {
    let path = format!("repos/{}/issues/{}", normalize_repo_path(repo), issue_number);
    if let Some(route_val) = st.routes.get_mut(&path) {
        if let Some(obj) = route_val.as_object_mut() {
            obj.insert("state".to_string(), serde_json::Value::String("closed".into()));
            obj.insert("state_reason".to_string(), serde_json::Value::String("completed".into()));
        }
    }
}

fn normalize_repo_path(repo: &str) -> String {
    let parts: Vec<&str> = repo.trim().split('/').collect();
    if parts.len() == 3 {
        format!("{}/{}", parts[1], parts[2])
    } else {
        repo.trim().to_string()
    }
}

fn pr_merge_method(args: &[String]) -> &'static str {
    if args.contains(&"--rebase".to_string()) {
        "REBASE"
    } else if args.contains(&"--merge".to_string()) {
        "MERGE"
    } else {
        "SQUASH"
    }
}

// --- Review comments ---

fn handle_pr_review_comments(st: &State, route: &str) -> Result<Option<bool>, Box<dyn std::error::Error>> {
    let (repo, pr_number, review_id) = parse_pr_review_comments_route(route);
    if review_id.is_empty() {
        return Ok(None);
    }

    if lookup_pull_request(st, &repo, pr_number, "").is_none() {
        return Ok(None);
    }

    let key = format!("{repo}#{pr_number}");
    if let Some(pr) = st.pull_requests.get(&key) {
        for review in &pr.reviews {
            if let Some(id) = review.get("id").and_then(|v| v.as_str()) {
                if id == review_id {
                    println!("[[]]");
                    return Ok(Some(true));
                }
            }
        }
    }
    Ok(None)
}

// --- Reviews ---

fn handle_pr_reviews(
    st: &State,
    args: &[String],
    stdin: &str,
    route: &str,
    state_path: &str,
) -> Result<Option<bool>, Box<dyn std::error::Error>> {
    let (repo, pr_number) = parse_pr_review_route(route);
    if pr_number == 0 {
        return Ok(None);
    }

    let raw_method = flag_value(args, "--method");
    let method = if raw_method.is_empty() { "GET".to_string() } else { raw_method.to_uppercase() };

    if method == "POST" {
        // Parse review create payload from stdin
        let payload: serde_json::Value = serde_json::from_str(stdin)?;
        let body = payload.get("body").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let event = payload.get("event").and_then(|v| v.as_str()).unwrap_or("").to_string();

        let mut st = st.clone();
        let key = format!("{repo}#{pr_number}");
        let pr = match st.pull_requests.get_mut(&key) {
            Some(p) => p,
            None => return Ok(None),
        };

        let review_id = format!("review-{}", pr.reviews.len() + 1);
        pr.reviews.push(serde_json::json!({
            "id": review_id,
            "body": body,
            "state": review_state_for_event(&event),
            "user": {"login": first_non_empty(&[&st.current_user_login, "looper"])}
        }));

        save_state(state_path, &st)?;
        println!("{{\"id\":\"{review_id}\"}}");
        return Ok(Some(true));
    }

    // GET — list reviews
    let key = format!("{repo}#{pr_number}");
    if let Some(pr) = st.pull_requests.get(&key) {
        let payload = serde_json::to_string(&vec![pr.reviews.clone()])?;
        println!("{payload}");
        return Ok(Some(true));
    }

    Ok(None)
}

fn parse_pr_review_route(route: &str) -> (String, i64) {
    const MARKER: &str = "repos/";
    if !route.starts_with(MARKER) || !route.ends_with("/reviews") {
        return (String::new(), 0);
    }
    let rest = route.strip_prefix(MARKER).unwrap_or("").strip_suffix("/reviews").unwrap_or("");
    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() < 4 || parts[2] != "pulls" {
        return (String::new(), 0);
    }
    let pr_number: i64 = match parts[3].parse() {
        Ok(n) => n,
        Err(_) => return (String::new(), 0),
    };
    (format!("{}/{}", parts[0], parts[1]), pr_number)
}

fn parse_pr_review_comments_route(route: &str) -> (String, i64, String) {
    const MARKER: &str = "repos/";
    if !route.starts_with(MARKER) || !route.contains("/reviews/") || !route.ends_with("/comments") {
        return (String::new(), 0, String::new());
    }
    let rest = route.strip_prefix(MARKER).unwrap_or("");
    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() < 7 || parts[2] != "pulls" || parts[4] != "reviews" {
        return (String::new(), 0, String::new());
    }
    let pr_number: i64 = match parts[3].parse() {
        Ok(n) => n,
        Err(_) => return (String::new(), 0, String::new()),
    };
    (format!("{}/{}", parts[0], parts[1]), pr_number, parts[5].to_string())
}

fn review_state_for_event(event: &str) -> &'static str {
    match event.to_uppercase().trim() {
        "APPROVE" => "APPROVED",
        "REQUEST_CHANGES" => "CHANGES_REQUESTED",
        _ => "COMMENTED",
    }
}

/// A hydrated PR with defaulted fields.
struct HydratedPR<'a> {
    inner: &'a PullRequestState,
    head_sha: String,
    base_sha: String,
    url: String,
    state: String,
    author: String,
    merge_state_status: String,
    mergeable_state: String,
    updated_at: String,
    created_at: String,
}

/// Look up a pull request by repo + number and return a hydrated version.
fn lookup_pull_request<'a>(st: &'a State, repo: &str, pr_number: i64, git_path: &str) -> Option<HydratedPR<'a>> {
    if !repo.is_empty() {
        let key = format!("{repo}#{pr_number}");
        if let Some(pr) = st.pull_requests.get(&key) {
            return Some(hydrate_pr(pr, git_path));
        }
    }
    for pr in st.pull_requests.values() {
        if pr.number == pr_number && (repo.is_empty() || pr.repo == repo) {
            return Some(hydrate_pr(pr, git_path));
        }
    }
    None
}

fn hydrate_pr<'a>(pr: &'a PullRequestState, git_path: &str) -> HydratedPR<'a> {
    let git_bin = first_non_empty(&[&env_var(ENV_GIT_PATH).unwrap_or_default(), git_path, "git"]);
    let head_sha = if !pr.git_dir.is_empty() {
        let ref_str = first_non_empty(&[&pr.head_ref, &pr.head_ref_name]);
        resolve_git_ref(&git_bin, &pr.git_dir, &ref_str).unwrap_or_else(|| pr.head_sha.clone())
    } else {
        pr.head_sha.clone()
    };
    let base_sha = if !pr.git_dir.is_empty() {
        let ref_str = first_non_empty(&[&pr.base_ref, &pr.base_ref_name]);
        resolve_git_ref(&git_bin, &pr.git_dir, &ref_str).unwrap_or_else(|| pr.base_sha.clone())
    } else {
        pr.base_sha.clone()
    };

    let state = if pr.state.is_empty() { "OPEN".to_string() } else { pr.state.clone() };
    let url = if pr.url.is_empty() && !pr.repo.is_empty() && pr.number > 0 {
        format!("https://github.com/{}/pull/{}", pr.repo, pr.number)
    } else {
        pr.url.clone()
    };
    let author = if pr.author.is_empty() { "octocat".to_string() } else { pr.author.clone() };
    let merge_state_status =
        if pr.merge_state_status.is_empty() { "CLEAN".to_string() } else { pr.merge_state_status.clone() };
    let mergeable_state =
        if pr.mergeable_state.is_empty() { merge_state_status.to_lowercase() } else { pr.mergeable_state.clone() };
    let updated_at = if pr.updated_at.is_empty() { "2026-05-12T00:00:00Z".to_string() } else { pr.updated_at.clone() };
    let created_at = if pr.created_at.is_empty() { updated_at.clone() } else { pr.created_at.clone() };

    HydratedPR {
        inner: pr,
        head_sha,
        base_sha,
        url,
        state,
        author,
        merge_state_status,
        mergeable_state,
        updated_at,
        created_at,
    }
}

fn pr_field_value(pr: &HydratedPR, field: &str) -> serde_json::Value {
    match field {
        "number" => serde_json::json!(pr.inner.number),
        "title" => serde_json::json!(pr.inner.title),
        "body" => serde_json::json!(pr.inner.body),
        "url" => serde_json::json!(pr.url),
        "state" => serde_json::json!(pr.state),
        "createdAt" => serde_json::json!(pr.created_at),
        "updatedAt" => serde_json::json!(pr.updated_at),
        "closedAt" => serde_json::json!(pr.inner.closed_at),
        "isDraft" => serde_json::json!(pr.inner.is_draft),
        "reviewDecision" => serde_json::json!(pr.inner.review_decision),
        "labels" => {
            let items: Vec<serde_json::Value> =
                pr.inner.labels.iter().map(|l| serde_json::json!({"name": l})).collect();
            serde_json::json!(items)
        }
        "headRefName" => serde_json::json!(pr.inner.head_ref_name),
        "baseRefName" => serde_json::json!(pr.inner.base_ref_name),
        "headRefOid" => serde_json::json!(pr.head_sha),
        "baseRefOid" => serde_json::json!(pr.base_sha),
        "author" => serde_json::json!({"login": pr.author}),
        "authorAssociation" => serde_json::json!(pr.inner.author_association),
        "reviewRequests" => {
            let items: Vec<serde_json::Value> = pr
                .inner
                .review_requests
                .iter()
                .map(|login| {
                    serde_json::json!({
                        "__typename": "ReviewRequest",
                        "requestedReviewer": {
                            "__typename": "User",
                            "login": login
                        }
                    })
                })
                .collect();
            serde_json::json!(items)
        }
        "comments" => serde_json::json!(pr.inner.issue_comments),
        "reviews" => serde_json::json!(pr.inner.reviews),
        "statusCheckRollup" => serde_json::json!(pr.inner.status_check_rollup),
        "mergeStateStatus" => serde_json::json!(pr.merge_state_status),
        "mergeable" => serde_json::json!(pr.inner.mergeable),
        "mergeable_state" => serde_json::json!(pr.mergeable_state),
        "merged_at" => serde_json::json!(pr.inner.merged_at),
        "auto_merge" => serde_json::json!(pr.inner.auto_merge),
        _ => default_value(field),
    }
}

// --- PR API handler ---

fn handle_pr_api(st: &State, route: &str, git_path: &str) -> Result<Option<bool>, Box<dyn std::error::Error>> {
    const MARKER: &str = "repos/";
    if !route.starts_with(MARKER) || !route.contains("/pulls/") || route.contains("/reviews") {
        return Ok(None);
    }
    let rest = route.strip_prefix(MARKER).unwrap_or("");
    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() < 4 || parts[2] != "pulls" {
        return Ok(None);
    }
    let pr_number: i64 = match parts[3].parse() {
        Ok(n) => n,
        Err(_) => return Ok(None),
    };

    let Some(pr) = lookup_pull_request(st, &format!("{}/{}", parts[0], parts[1]), pr_number, git_path) else {
        return Ok(None);
    };

    let payload = serde_json::json!({
        "number": pr.inner.number,
        "title": pr.inner.title,
        "body": pr.inner.body,
        "url": pr.url,
        "html_url": pr.url,
        "state": pr.state.to_lowercase(),
        "created_at": pr.created_at,
        "updated_at": pr.updated_at,
        "closed_at": pr.inner.closed_at,
        "merged_at": pr.inner.merged_at,
        "labels": pr.inner.labels.iter().map(|l| serde_json::json!({"name": l})).collect::<Vec<_>>(),
        "head": {"ref": pr.inner.head_ref_name, "sha": pr.head_sha},
        "base": {"ref": pr.inner.base_ref_name, "sha": pr.base_sha},
        "mergeable": pr.inner.mergeable,
        "mergeable_state": first_non_empty(&[&pr.mergeable_state, &pr.merge_state_status]),
        "auto_merge": pr.inner.auto_merge,
    });
    println!("{payload}");
    Ok(Some(true))
}

// --- Check runs API handler ---

fn handle_check_runs_api(st: &State, route: &str) -> Result<Option<bool>, Box<dyn std::error::Error>> {
    const MARKER: &str = "repos/";
    if !route.starts_with(MARKER) || !route.contains("/commits/") || !route.ends_with("/check-runs") {
        return Ok(None);
    }
    let rest = route.strip_prefix(MARKER).unwrap_or("");
    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() < 5 || parts[2] != "commits" {
        return Ok(None);
    }
    let repo = format!("{}/{}", parts[0], parts[1]);
    let ref_sha = parts[3].to_string();

    for pr in st.pull_requests.values() {
        if pr.repo == repo && pr.head_sha == ref_sha {
            let payload = serde_json::json!({
                "total_count": pr.check_runs.len(),
                "check_runs": pr.check_runs,
            });
            println!("{payload}");
            return Ok(Some(true));
        }
    }
    Ok(None)
}

// --- GraphQL ---

fn handle_graphql_state(
    st: &State,
    args: &[String],
    stdin: &str,
    state_path: &str,
    git_path: &str,
) -> Result<Option<bool>, Box<dyn std::error::Error>> {
    let query = format!("{} {}", args.join(" "), stdin);

    if query.contains("addPullRequestReviewThreadReply") {
        let thread_id = field_value(args, "threadId");
        let body = field_value(args, "body");
        if thread_id.is_empty() {
            return Ok(None);
        }
        let comment_id = append_thread_reply(st, &thread_id, &body, state_path)?;
        if comment_id.is_empty() {
            return Ok(None);
        }
        println!("{{\"data\":{{\"addPullRequestReviewThreadReply\":{{\"comment\":{{\"id\":\"{comment_id}\"}}}}}}}}");
        return Ok(Some(true));
    }

    if query.contains("unresolveReviewThread") {
        let thread_id = field_value(args, "threadId");
        set_thread_resolved(st, &thread_id, false, state_path)?;
        println!(
            "{{\"data\":{{\"unresolveReviewThread\":{{\"thread\":{{\"id\":\"{thread_id}\",\"isResolved\":false}}}}}}}}"
        );
        return Ok(Some(true));
    }

    if query.contains("resolveReviewThread") {
        let thread_id = field_value(args, "threadId");
        set_thread_resolved(st, &thread_id, true, state_path)?;
        println!(
            "{{\"data\":{{\"resolveReviewThread\":{{\"thread\":{{\"id\":\"{thread_id}\",\"isResolved\":true}}}}}}}}"
        );
        return Ok(Some(true));
    }

    if query.contains("reviewThreads(") {
        let repo = repo_from_graphql_args(args);
        let pr_number: i64 = field_value(args, "prNumber").parse().unwrap_or(0);
        if let Some(pr) = lookup_pull_request(st, &repo, pr_number, git_path) {
            let payload = serde_json::json!({
                "data": {
                    "repository": {
                        "pullRequest": {
                            "reviewThreads": {
                                "nodes": review_thread_nodes(&pr),
                                "pageInfo": {"hasNextPage": false, "endCursor": ""}
                            }
                        }
                    }
                }
            });
            println!("{payload}");
            return Ok(Some(true));
        }
        return Ok(None);
    }

    if query.contains("PullRequestReviewThread") || query.contains("node(id: $threadId)") {
        let thread_id = field_value(args, "threadId");
        if let Some(thread) = lookup_thread(st, &thread_id) {
            let payload = serde_json::json!({
                "data": {
                    "node": {
                        "id": thread.id,
                        "isResolved": thread.is_resolved,
                        "comments": {
                            "nodes": review_comment_nodes(&thread.comments),
                            "pageInfo": {"hasNextPage": false, "endCursor": ""}
                        }
                    }
                }
            });
            println!("{payload}");
            return Ok(Some(true));
        }
        return Ok(None);
    }

    Ok(None)
}

fn graphql_operation(args: &[String], stdin: &str) -> String {
    let combined = format!("{} {}", args.join(" "), stdin);
    for token in combined.split_whitespace() {
        if token.contains("unresolveReviewThread") {
            return "unresolveReviewThread".to_string();
        }
        if token.contains("resolveReviewThread") {
            return "resolveReviewThread".to_string();
        }
    }
    "default".to_string()
}

fn review_thread_nodes(pr: &HydratedPR) -> Vec<serde_json::Value> {
    pr.inner
        .threads
        .iter()
        .map(|t| {
            serde_json::json!({
                "id": t.id,
                "isResolved": t.is_resolved,
                "path": t.path,
                "line": t.line,
                "comments": {
                    "nodes": review_comment_nodes(&t.comments),
                    "pageInfo": {"hasNextPage": false, "endCursor": ""}
                }
            })
        })
        .collect()
}

fn review_comment_nodes(comments: &[ReviewCommentState]) -> Vec<serde_json::Value> {
    comments
        .iter()
        .map(|c| {
            serde_json::json!({
                "id": c.id,
                "body": c.body,
                "createdAt": first_non_empty(&[&c.created_at, "2026-05-12T00:00:00Z"]),
                "updatedAt": first_non_empty(&[&c.updated_at, "2026-05-12T00:00:00Z"]),
                "path": c.path,
                "line": c.line,
                "url": c.url,
                "author": {"login": first_non_empty(&[&c.author, "octocat"])},
                "originalCommit": {"oid": first_non_empty(&[&c.original_commit_oid, &c.commit_oid])},
                "commit": {"oid": first_non_empty(&[&c.commit_oid, &c.original_commit_oid])},
            })
        })
        .collect()
}

fn lookup_thread<'a>(st: &'a State, thread_id: &str) -> Option<&'a ReviewThreadState> {
    for pr in st.pull_requests.values() {
        for thread in &pr.threads {
            if thread.id == thread_id {
                return Some(thread);
            }
        }
    }
    None
}

fn set_thread_resolved(
    st: &State,
    thread_id: &str,
    resolved: bool,
    state_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut st = st.clone();
    for (_key, pr) in st.pull_requests.iter_mut() {
        for thread in pr.threads.iter_mut() {
            if thread.id == thread_id {
                thread.is_resolved = resolved;
                return save_state(state_path, &st);
            }
        }
    }
    Err(format!("review thread not found: {thread_id}").into())
}

fn append_thread_reply(
    st: &State,
    thread_id: &str,
    body: &str,
    state_path: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut st = st.clone();
    for (_key, pr) in st.pull_requests.iter_mut() {
        for thread in pr.threads.iter_mut() {
            if thread.id == thread_id {
                let comment_id = format!("reply-{}", thread.comments.len() + 1);
                let login = first_non_empty(&[&st.current_user_login, "looper"]);
                thread.comments.push(ReviewCommentState {
                    id: comment_id.clone(),
                    body: body.to_string(),
                    author: login.clone(),
                    created_at: "2026-05-12T00:00:00Z".to_string(),
                    updated_at: "2026-05-12T00:00:00Z".to_string(),
                    path: thread.path.clone(),
                    line: thread.line,
                    original_commit_oid: String::new(),
                    commit_oid: String::new(),
                    url: format!("https://example.test/threads/{}#{}", thread_id, comment_id),
                });
                save_state(state_path, &st)?;
                return Ok(comment_id);
            }
        }
    }
    Err(format!("review thread not found: {thread_id}").into())
}

// --- Compare ---

fn build_compare_payload(route: &str, git_path: &str) -> Option<String> {
    const MARKER: &str = "/compare/";
    let idx = route.find(MARKER)?;
    let comparison = &route[idx + MARKER.len()..];
    let parts: Vec<&str> = comparison.splitn(2, "...").collect();
    if parts.len() != 2 {
        return None;
    }
    let base = parts[0];
    let head = parts[1];
    let git_bin = first_non_empty(&[&env_var(ENV_GIT_PATH).unwrap_or_default(), git_path, "git"]);

    let output =
        Command::new(&git_bin).args(["rev-list", "--left-right", "--count", &format!("{base}...{head}")]).output();
    let output = match output {
        Ok(o) => o,
        Err(_) => {
            return Some(r#"{"ahead_by":0,"behind_by":0,"status":"identical","total_commits":0}"#.to_string());
        }
    };
    if !output.status.success() {
        return Some(r#"{"ahead_by":0,"behind_by":0,"status":"identical","total_commits":0}"#.to_string());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let fields: Vec<&str> = stdout.trim().split_whitespace().collect();
    if fields.len() != 2 {
        return Some(r#"{"ahead_by":0,"behind_by":0,"status":"identical","total_commits":0}"#.to_string());
    }
    let behind: i64 = fields[0].parse().unwrap_or(0);
    let ahead: i64 = fields[1].parse().unwrap_or(0);
    let status = if ahead > 0 && behind > 0 {
        "diverged"
    } else if ahead > 0 {
        "ahead"
    } else if behind > 0 {
        "behind"
    } else {
        "identical"
    };
    Some(format!(
        r#"{{"ahead_by":{ahead},"behind_by":{behind},"status":"{status}","total_commits":{}}}"#,
        ahead + behind
    ))
}

// ---------------------------------------------------------------------------
// Generic helpers
// ---------------------------------------------------------------------------

fn resolve_git_ref(git_path: &str, git_dir: &str, r#ref: &str) -> Option<String> {
    if git_dir.is_empty() || r#ref.is_empty() {
        return None;
    }
    let output = Command::new(git_path).args(["--git-dir", git_dir, "rev-parse", r#ref]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn command_key(args: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if takes_value(arg) {
            skip_next = true;
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        parts.push(arg.clone());
        if parts.len() == 2 {
            break;
        }
    }
    parts.join(" ")
}

fn first_non_flag(args: &[String]) -> String {
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if takes_value(arg) {
            skip_next = true;
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return arg.clone();
    }
    String::new()
}

fn flag_value<'a>(args: &'a [String], name: &str) -> &'a str {
    let eq_prefix = format!("{name}=");
    for (i, arg) in args.iter().enumerate() {
        if arg == name && i + 1 < args.len() {
            return &args[i + 1];
        }
        if let Some(val) = arg.strip_prefix(&eq_prefix) {
            return val;
        }
    }
    ""
}

fn has_arg(args: &[String], flag: &str, value: &str) -> bool {
    for (i, arg) in args.iter().enumerate() {
        if arg == flag && i + 1 < args.len() && args[i + 1] == value {
            return true;
        }
    }
    false
}

fn first_non_empty(values: &[&str]) -> String {
    for v in values {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    String::new()
}

fn takes_value(flag: &str) -> bool {
    if flag.contains('=') {
        return false;
    }
    matches!(
        flag,
        "-X" | "--method"
            | "-f"
            | "-F"
            | "--field"
            | "--raw-field"
            | "-H"
            | "--header"
            | "--hostname"
            | "--repo"
            | "--json"
            | "--jq"
            | "--template"
            | "--input"
    )
}

fn requested_json_fields(args: &[String]) -> Vec<String> {
    for (i, arg) in args.iter().enumerate() {
        if arg == "--json" && i + 1 < args.len() {
            return split_fields(&args[i + 1]);
        }
        if let Some(val) = arg.strip_prefix("--json=") {
            return split_fields(val);
        }
    }
    Vec::new()
}

fn split_fields(raw: &str) -> Vec<String> {
    raw.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

fn parse_pr_number(args: &[String]) -> Option<i64> {
    for arg in args {
        if arg.starts_with('-') {
            continue;
        }
        if let Ok(n) = arg.parse::<i64>() {
            return Some(n);
        }
    }
    None
}

fn field_value(args: &[String], key: &str) -> String {
    let prefix = format!("{key}=");
    for (i, arg) in args.iter().enumerate() {
        if (arg == "-F" || arg == "-f") && i + 1 < args.len() {
            if let Some(val) = args[i + 1].strip_prefix(&prefix) {
                return val.to_string();
            }
        }
        if let Some(val) = arg.strip_prefix("-F") {
            if let Some(val) = val.strip_prefix(&prefix) {
                return val.to_string();
            }
        }
    }
    String::new()
}

fn repo_from_graphql_args(args: &[String]) -> String {
    let owner = field_value(args, "owner");
    let name = field_value(args, "name");
    if owner.is_empty() || name.is_empty() {
        return String::new();
    }
    format!("{owner}/{name}")
}

fn default_value(field: &str) -> serde_json::Value {
    match field {
        "number" => serde_json::json!(1),
        "title" => serde_json::json!("fake title"),
        "state" => serde_json::json!("OPEN"),
        "url" => serde_json::json!("https://example.test/owner/repo/pull/1"),
        "id" => serde_json::json!("FAKE_node_id"),
        "body" => serde_json::json!(""),
        "headRefName" => serde_json::json!("fake-branch"),
        "headRefOid" => serde_json::json!("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"),
        "baseRefOid" => serde_json::json!("basebeefdeadbeefdeadbeefdeadbeefdeadbeef"),
        "author" => serde_json::json!({"login": "octocat"}),
        "reviewRequests" | "reviews" | "comments" | "statusCheckRollup" | "labels" | "assignees" => {
            serde_json::json!([])
        }
        "authorAssociation" => serde_json::json!("NONE"),
        "updatedAt" => serde_json::json!("2026-05-12T00:00:00Z"),
        "createdAt" => serde_json::json!("2026-05-12T00:00:00Z"),
        "closedAt" => serde_json::json!(""),
        "isDraft" => serde_json::json!(false),
        "reviewDecision" => serde_json::json!(""),
        "mergeStateStatus" => serde_json::json!("CLEAN"),
        _ => serde_json::json!(field),
    }
}

fn validate_fields(command: &str, fields: &[String], allowed: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let allow: std::collections::HashSet<&str> = allowed.iter().map(|s| s.as_str()).collect();
    for field in fields {
        if allow.contains(field.as_str()) {
            continue;
        }
        let mut avail: Vec<&str> = allowed.iter().map(|s| s.as_str()).collect();
        avail.sort_unstable();
        return Err(format!("unknown JSON field: {:?}\nAvailable fields:\n  {}\n", field, avail.join("\n  ")).into());
    }
    let _ = command;
    Ok(())
}

fn emit_response(resp: &Response) -> Result<(), Box<dyn std::error::Error>> {
    if !resp.stderr.is_empty() {
        eprint!("{}", resp.stderr);
    }
    if let Some(ref stdout_val) = resp.stdout {
        emit_json_value(stdout_val);
    }
    if resp.exit_code != 0 {
        process::exit(resp.exit_code);
    }
    Ok(())
}

fn emit_json_value(val: &serde_json::Value) {
    // If it's a JSON string, try to unwrap and print as a raw string.
    if let Some(text) = val.as_str() {
        print!("{text}");
        if text.is_empty() || !text.ends_with('\n') {
            println!();
        }
    } else {
        let text = serde_json::to_string(val).unwrap_or_default();
        print!("{text}");
        if !text.ends_with('\n') {
            println!();
        }
    }
}

fn emit_default_json(key: &str, fields: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut object = serde_json::Map::new();
    for field in fields {
        object.insert(field.clone(), default_value(field));
    }
    let payload: serde_json::Value =
        if key.ends_with("list") { serde_json::json!([object]) } else { serde_json::json!(object) };
    println!("{payload}");
    Ok(())
}

// ---------------------------------------------------------------------------
// I/O helpers
// ---------------------------------------------------------------------------

fn load_schema(path: &str) -> Result<Schema, Box<dyn std::error::Error>> {
    if path.is_empty() {
        return Ok(Schema { json_field_allowlist: HashMap::new() });
    }
    let payload = std::fs::read_to_string(path)?;
    let mut decoded: Schema = serde_json::from_str(&payload)?;
    if decoded.json_field_allowlist.is_empty() {
        decoded.json_field_allowlist = HashMap::new();
    }
    Ok(decoded)
}

fn load_state(path: &str) -> Result<State, Box<dyn std::error::Error>> {
    if path.is_empty() {
        return Ok(State::default());
    }
    let payload = match std::fs::read_to_string(path) {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(State::default());
        }
        Err(e) => return Err(e.into()),
    };
    let decoded: State = serde_json::from_str(&payload)?;
    Ok(decoded)
}

fn save_state(path: &str, st: &State) -> Result<(), Box<dyn std::error::Error>> {
    if path.is_empty() {
        return Ok(());
    }
    let payload = serde_json::to_string_pretty(st)?;
    std::fs::write(path, payload)?;
    Ok(())
}

fn append_jsonl<P: AsRef<Path>>(path: P, value: &impl Serialize) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.as_ref().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = std::fs::OpenOptions::new().create(true).append(true).open(path.as_ref())?;
    let mut writer = BufWriter::new(file);
    let line = serde_json::to_string(value)?;
    writeln!(writer, "{line}")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Utility
// ---------------------------------------------------------------------------

fn env_var(name: &str) -> Option<String> {
    let val = env::var(name).ok()?;
    let trimmed = val.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn collect_env() -> HashMap<String, String> {
    let keys = [ENV_MODE, ENV_ARTIFACT_DIR, ENV_SCHEMA_PATH, ENV_STATE_PATH, ENV_RECORD_PATH, ENV_GIT_PATH, "HOME"];
    let mut m = HashMap::new();
    for key in &keys {
        if let Some(val) = env_var(key) {
            m.insert(key.to_string(), val);
        }
    }
    m
}

fn iso_timestamp() -> String {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => {
            let secs = d.as_secs() as i64;
            let nanos = d.subsec_nanos();
            let dt = chrono::DateTime::from_timestamp(secs, nanos).unwrap_or_default();
            format!("{}Z", dt.format("%Y-%m-%dT%H:%M:%S%.9f"))
        }
        Err(_) => "1970-01-01T00:00:00.000000000Z".to_string(),
    }
}

fn must_getwd() -> String {
    env::current_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default()
}

fn fatalf(code: i32, msg: &str) -> ! {
    eprint!("{msg}");
    process::exit(code);
}

// ---------------------------------------------------------------------------
// Default impls
// ---------------------------------------------------------------------------

impl Default for State {
    fn default() -> Self {
        Self {
            commands: HashMap::new(),
            routes: HashMap::new(),
            graphql: HashMap::new(),
            current_user_login: String::new(),
            pull_requests: HashMap::new(),
        }
    }
}

impl Default for Response {
    fn default() -> Self {
        Self { stdout: None, stderr: String::new(), exit_code: 0 }
    }
}

impl Default for Schema {
    fn default() -> Self {
        Self { json_field_allowlist: HashMap::new() }
    }
}
