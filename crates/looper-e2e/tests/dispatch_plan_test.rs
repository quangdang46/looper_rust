//! Dispatch/plan E2E integration test.
//!
//! Exercises the full dispatch/plan → looper:plan → planner → PR → spec-reviewing
//! pipeline through the fake-gh and fake-agent binaries.
//!
//! Steps:
//! 1. Start fake-gh, fake-agent
//! 2. Initiate daemon
//! 3. Add project with a seeded repo
//! 4. Set up fake-gh state with a `dispatch/plan` issue
//! 5. Wait for the coordinator tick to add `looper:plan` label
//! 6. Wait for the planner to create a PR
//! 7. Verify `looper:spec-reviewing` label was applied to the issue
//!
//! These tests require E2E binary paths set via environment variables.
//! They are skipped when the variables are not set (e.g. during `cargo test`).

use std::collections::HashMap;
use std::time::Duration;

use looper_e2e::binaries::BuiltBinaries;
use looper_e2e::config::{default_config, write_config, ConfigOptions, TestToolPaths};
use looper_e2e::daemon::start_looperd;
use looper_e2e::fake_gh::{FakeGH, GHPullRequest, GHState};
use looper_e2e::git::{create_seeded_repo, run_git};
use looper_e2e::ports::must_free_port;
use looper_e2e::temp_home::TempHome;
use looper_e2e::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn require_bins() -> Option<BuiltBinaries> {
    BuiltBinaries::from_env()
}

/// Poll the daemon's `/health` endpoint until it responds with HTTP 200
/// and `ok: true`, or until `timeout` elapses.
fn wait_for_health_ok(base_url: &str, timeout: Duration) -> Result<serde_json::Value, String> {
    let deadline = std::time::Instant::now() + timeout;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
        .map_err(|e| format!("build http client: {e}"))?;

    let health_url = format!("{base_url}/health");
    loop {
        if std::time::Instant::now() > deadline {
            return Err("timeout waiting for /health".into());
        }
        match client.get(&health_url).send() {
            Ok(resp) if resp.status().is_success() => {
                let body: serde_json::Value = resp.json().map_err(|e| format!("parse /health response: {e}"))?;
                if body.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                    return Ok(body);
                }
            }
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Poll fake-gh state for a specific issue to check its labels.
///
/// Returns the labels when found, or `None` on timeout.
fn poll_fake_gh_labels(
    state_path: &std::path::Path,
    _repo: &str,
    _issue_number: i64,
    timeout: Duration,
) -> Option<Vec<String>> {
    use looper_e2e::fake_gh::GHState;

    let deadline = std::time::Instant::now() + timeout;
    loop {
        if std::time::Instant::now() > deadline {
            return None;
        }
        if !state_path.exists() {
            std::thread::sleep(Duration::from_millis(200));
            continue;
        }
        let content = std::fs::read_to_string(state_path).ok()?;
        let state: GHState = serde_json::from_str(&content).ok()?;

        // Check if pull_requests contain the labels we care about
        // The coordinator adds labels via the fake-gh interface, which
        // updates the fake-gh state file.
        if let Some(ref prs) = state.pull_requests {
            for (_key, pr) in prs {
                if let Some(ref labels) = pr.labels {
                    return Some(labels.clone());
                }
            }
        }

        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Poll fake-gh state for a PR with the `looper:spec-reviewing` label.
fn poll_for_spec_reviewing(state_path: &std::path::Path, timeout: Duration) -> Option<Vec<String>> {
    use looper_e2e::fake_gh::GHState;

    let deadline = std::time::Instant::now() + timeout;
    loop {
        if std::time::Instant::now() > deadline {
            return None;
        }
        if !state_path.exists() {
            std::thread::sleep(Duration::from_millis(200));
            continue;
        }
        let content = std::fs::read_to_string(state_path).ok()?;
        let state: GHState = serde_json::from_str(&content).ok()?;

        if let Some(ref commands) = state.commands {
            // The fake-gh may record label-add operations in the commands record;
            // look at routes for label state
            if let Some(ref prs) = state.pull_requests {
                for (_key, pr) in prs {
                    if let Some(ref labels) = pr.labels {
                        if labels.iter().any(|l| l.contains("looper:spec-reviewing")) {
                            return Some(labels.clone());
                        }
                    }
                }
            }
        }

        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Create a temporary bin directory with symlinks so that `claude` and `gh`
/// resolve to the test's fake binaries.
fn symlink_test_bins(bins: &BuiltBinaries) -> (std::path::PathBuf, String) {
    let dir = std::env::temp_dir().join(format!("test-bin-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("create test bin dir");

    // Symlink claude -> fake-agent
    std::os::unix::fs::symlink(&bins.fake_agent_path, &dir.join("claude")).expect("symlink claude -> fake-agent");

    // Symlink gh -> fake-gh
    std::os::unix::fs::symlink(&bins.fake_gh_path, &dir.join("gh")).expect("symlink gh -> fake-gh");

    let path_val = format!("{}:{}", dir.display(), std::env::var("PATH").unwrap_or_default());
    (dir, path_val)
}

/// Build an issue detail JSON that fake-gh returns for the `/issues/1` route.
fn issue_with_dispatch_plan() -> serde_json::Value {
    serde_json::json!({
        "number": 1,
        "title": "Implement the widget system",
        "body": "## Acceptance Criteria\n\n- [ ] Widgets can be created\n- [ ] Widgets persist across restarts\n\nSpec: docs/specs/widget-system.md",
        "state": "open",
        "labels": [{"name": "dispatch/plan"}],
        "user": {"login": "e2e-test-user"},
        "created_at": "2026-06-22T10:00:00Z",
        "updated_at": "2026-06-22T10:00:00Z"
    })
}

/// Build a fake-gh state that pre-seeds a `dispatch/plan` issue.
fn build_fake_gh_state(origin_url: &str) -> GHState {
    GHState {
        current_user_login: Some("e2e-test-user".into()),
        commands: Some(serde_json::json!({
            "auth status": {
                "stdout": null,
                "stderr": "",
                "exitCode": 0
            }
        })),
        routes: Some(serde_json::json!({
            "repos/test-owner/test-repo/issues/1": issue_with_dispatch_plan()
        })),
        pull_requests: Some(HashMap::from([(
            "test-owner/test-repo#1".to_string(),
            GHPullRequest {
                number: 1,
                repo: Some("test-owner/test-repo".into()),
                title: Some("feat: implement widget system".into()),
                body: Some("Initial implementation of widget system\n\nCloses #1".into()),
                state: Some("OPEN".into()),
                head_ref_name: Some("feat/widget-system".into()),
                base_ref_name: Some("main".into()),
                author: Some("e2e-test-user".into()),
                merge_state_status: Some("CLEAN".into()),
                mergeable: Some(true),
                is_draft: Some(false),
                review_decision: Some("REVIEW_REQUIRED".into()),
                created_at: Some("2026-06-22T10:00:00Z".into()),
                updated_at: Some("2026-06-22T10:00:00Z".into()),
                ..Default::default()
            },
        )])),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[test]
fn test_dispatch_plan_to_implementation() {
    let bins = match require_bins() {
        Some(b) => b,
        None => {
            eprintln!("SKIP: E2E binary env vars not set");
            return;
        }
    };

    // -----------------------------------------------------------------------
    // 1. Create TempHome with seeded git repo + bare origin
    // -----------------------------------------------------------------------
    let home = TempHome::new("dispatch_plan");
    let port = must_free_port();
    let git_path = "git";

    let seeded = create_seeded_repo(git_path);

    let origin_path = std::env::temp_dir().join(format!("origin-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&origin_path).expect("create origin dir");
    run_git(git_path, &origin_path, &["init", "--bare"]);

    run_git(git_path, std::path::Path::new(&seeded.path), &["remote", "add", "origin", origin_path.to_str().unwrap()]);
    run_git(git_path, std::path::Path::new(&seeded.path), &["push", "origin", "main"]);

    let origin_url = origin_path.to_string_lossy().to_string();

    // -----------------------------------------------------------------------
    // 2. Configure fake-gh with dispatch/plan issue
    // -----------------------------------------------------------------------
    let schema = fake_gh::GHSchema {
        json_field_allowlist: HashMap::from([
            (
                "pr view".to_string(),
                vec![
                    "number",
                    "title",
                    "body",
                    "url",
                    "state",
                    "createdAt",
                    "updatedAt",
                    "headRefName",
                    "baseRefName",
                    "headRefOid",
                    "baseRefOid",
                    "author",
                    "mergeStateStatus",
                    "isDraft",
                    "reviewDecision",
                    "labels",
                    "reviewRequests",
                    "comments",
                    "reviews",
                    "statusCheckRollup",
                ]
                .into_iter()
                .map(String::from)
                .collect(),
            ),
            (
                "pr list".to_string(),
                vec![
                    "number",
                    "title",
                    "url",
                    "state",
                    "updatedAt",
                    "isDraft",
                    "reviewDecision",
                    "labels",
                    "headRefName",
                    "baseRefName",
                    "headRefOid",
                    "baseRefOid",
                    "author",
                    "reviewRequests",
                    "reviews",
                    "mergeStateStatus",
                ]
                .into_iter()
                .map(String::from)
                .collect(),
            ),
            (
                "issue list".to_string(),
                vec!["number", "title", "url", "state", "updatedAt", "labels", "author", "body"]
                    .into_iter()
                    .map(String::from)
                    .collect(),
            ),
        ]),
    };

    let fgh = FakeGH::new(&bins, schema);
    let state = build_fake_gh_state(&origin_url);
    fgh.write_state(&state);

    // -----------------------------------------------------------------------
    // 3. Configure fake-agent (planning mode: write a spec file)
    // -----------------------------------------------------------------------
    let fake_agent = FakeAgent::new(&bins);

    // The fake-agent writes a spec file when invoked for planning
    let write_file = home.working_dir.join("docs").join("specs").join("widget-system.md");

    let (_vendor, _agent_bin, mut agent_env) = fake_agent.agent_config(
        "write-file", // mode: write a file and succeed
        "/usr/bin/git",
        fgh.path.to_str().unwrap(),
    );

    agent_env.insert(
        looper_e2e::fake_agent::ENV_FAKE_AGENT_WRITE_FILE.to_string(),
        write_file.to_string_lossy().to_string(),
    );

    // -----------------------------------------------------------------------
    // 4. Create symlinks
    // -----------------------------------------------------------------------
    let (bin_dir, path_val) = symlink_test_bins(&bins);

    let fgh_env = fgh.env_map();

    let extra_env: Vec<(&str, &str)> = vec![
        ("PATH", &path_val),
        (
            looper_e2e::fake_gh::ENV_FAKE_GH_MODE,
            fgh_env.get(looper_e2e::fake_gh::ENV_FAKE_GH_MODE).map(|s| s.as_str()).unwrap_or("strict"),
        ),
        (
            looper_e2e::fake_gh::ENV_FAKE_GH_ARTIFACT_DIR,
            fgh_env.get(looper_e2e::fake_gh::ENV_FAKE_GH_ARTIFACT_DIR).expect("fake-gh artifact dir"),
        ),
        (
            looper_e2e::fake_gh::ENV_FAKE_GH_STATE_PATH,
            fgh_env.get(looper_e2e::fake_gh::ENV_FAKE_GH_STATE_PATH).expect("fake-gh state path"),
        ),
        (
            looper_e2e::fake_gh::ENV_FAKE_GH_SCHEMA_PATH,
            fgh_env.get(looper_e2e::fake_gh::ENV_FAKE_GH_SCHEMA_PATH).expect("fake-gh schema path"),
        ),
        (
            looper_e2e::fake_gh::ENV_FAKE_GH_RECORD_PATH,
            fgh_env.get(looper_e2e::fake_gh::ENV_FAKE_GH_RECORD_PATH).expect("fake-gh record path"),
        ),
        (looper_e2e::fake_agent::ENV_FAKE_AGENT_MODE, "write-file"),
        (looper_e2e::fake_agent::ENV_FAKE_AGENT_ARTIFACT_DIR, fake_agent.artifact_dir.to_str().expect("artifact dir")),
        (looper_e2e::fake_agent::ENV_FAKE_AGENT_STATE_PATH, fake_agent.state_path.to_str().expect("state path")),
        (looper_e2e::fake_agent::ENV_FAKE_AGENT_WRITE_FILE, write_file.to_str().expect("write file")),
        (looper_e2e::fake_agent::ENV_FAKE_AGENT_GIT_PATH, "/usr/bin/git"),
        (looper_e2e::fake_agent::ENV_FAKE_AGENT_GH_PATH, fgh.path.to_str().expect("fake-gh path")),
    ];

    // -----------------------------------------------------------------------
    // 5. Write config with a project
    // -----------------------------------------------------------------------
    let _cfg = default_config(
        &home,
        ConfigOptions {
            port: Some(port),
            working_dir: Some(home.working_dir.to_string_lossy().to_string()),
            agent_vendor: Some(looper_types::AgentVendor::Claude),
            agent_command: Some(bins.fake_agent_path.to_string_lossy().to_string()),
            agent_env: agent_env.clone(),
            tool_paths: TestToolPaths {
                git: "git".to_string(),
                gh: "gh".to_string(),
                looper: bins.looper_path.to_string_lossy().to_string(),
                osascript: bins.fake_osascript_path.to_string_lossy().to_string(),
            },
            projects: vec![serde_json::json!({
                "name": "test-project",
                "path": seeded.path,
                "repo_url": origin_url,
                "default_branch": "main",
                "enabled": true,
            })],
            ..Default::default()
        },
    );

    let overrides = HashMap::from([
        (
            "defaults".to_string(),
            serde_json::json!({
                "home-dir": home.home_dir.to_string_lossy().to_string(),
            }),
        ),
        (
            "daemon".to_string(),
            serde_json::json!({
                "log-file": home.log_dir.join("looperd.log").to_string_lossy().to_string(),
            }),
        ),
    ]);

    write_config(&home.config_path, &_cfg, overrides);

    // -----------------------------------------------------------------------
    // 6. Start looperd
    // -----------------------------------------------------------------------
    let daemon = start_looperd(&bins, &home, home.config_path.to_str().unwrap(), &extra_env, "127.0.0.1", port);

    let base = daemon.base_url().to_string();

    // -----------------------------------------------------------------------
    // 7. Wait for daemon ready
    // -----------------------------------------------------------------------
    let result = wait_for_health_ok(&base, Duration::from_secs(30));
    assert!(result.is_ok(), "daemon should become ready within 30s: {:?}", result.err());

    // -----------------------------------------------------------------------
    // 8. Add project via API (in case config didn't register it)
    // -----------------------------------------------------------------------
    let client = reqwest::blocking::Client::new();
    let add_body = serde_json::json!({
        "name": "dispatch-test-project",
        "repo_url": origin_path.to_string_lossy().to_string(),
        "default_branch": "main",
        "enabled": true,
    });
    let _add_resp = client.post(format!("{base}/api/projects")).json(&add_body).send();

    // -----------------------------------------------------------------------
    // 9. Wait for the coordinator to pick up the dispatch/plan issue
    //    The coordinator runs on ~5s poll interval, so we wait up to 60s.
    // -----------------------------------------------------------------------
    //
    // The coordinator will:
    //   a. Discover issues with `dispatch/plan` label
    //   b. Add `looper:plan` label to trigger the planner
    //   c. The planner creates a spec PR with `looper:spec-reviewing`
    //
    // We check the fake-gh state for evidence of the spec-reviewing label
    // being applied.

    let state_path = &fgh.state_path;
    let labels = poll_for_spec_reviewing(state_path, Duration::from_secs(120));

    assert!(
        labels.is_some(),
        "Timed out waiting for looper:spec-reviewing label to appear in fake-gh state.\n\
         Daemon stderr:\n{}\n\
         Daemon stdout:\n{}",
        daemon.read_stderr(),
        daemon.read_stdout(),
    );

    let labels = labels.unwrap();
    assert!(
        labels.iter().any(|l| l.contains("looper:spec-reviewing")),
        "Expected looper:spec-reviewing label, got: {:?}",
        labels
    );

    println!("SUCCESS: looper:spec-reviewing label found in fake-gh state: {:?}", labels);

    // -----------------------------------------------------------------------
    // 10. Clean shutdown
    // -----------------------------------------------------------------------
    let _ = client.post(format!("{base}/shutdown")).send();
    std::thread::sleep(Duration::from_millis(500));
    daemon.stop(Duration::from_secs(5));

    // Cleanup
    let _ = std::fs::remove_dir_all(&bin_dir);
    let _ = std::fs::remove_dir_all(&origin_path);
    home.cleanup();
}
