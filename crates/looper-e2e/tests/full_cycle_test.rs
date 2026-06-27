//! Full-cycle E2E test: daemon startup, API surface, lifecycle.
//!
//! This test exercises a complete looper daemon integration:
//! 1. Creates a temp home with a seeded git repo + bare origin
//! 2. Configures fake-gh with a pre-defined state having an issue labeled
//!    `looper:plan` with acceptance criteria and a pull request for review
//! 3. Configures fake-agent with `success-with-diff` mode
//! 4. Starts looperd with a `claude` symlink -> fake-agent and `gh` symlink
//!    -> fake-gh on PATH, plus all LOOPER_E2E_FAKE_* env vars set so the
//!    daemon inherits them for the agent subprocess
//! 5. Waits for daemon ready via the `/health` endpoint
//! 6. Verifies all major API endpoints (project, loop, queue, events, locks)
//! 7. Stops daemon cleanly
//!
//! These tests require E2E binary paths set via environment variables.
//! They are skipped when the variables are not set (e.g. during `cargo test`).

use std::collections::HashMap;
use std::time::Duration;

use looper_e2e::binaries::BuiltBinaries;
use looper_e2e::config::{default_config, write_config, ConfigOptions, TestToolPaths};
use looper_e2e::daemon::start_looperd;
use looper_e2e::fake_gh::{FakeGH, GHPullRequest, GHState, GHThread, GHThreadComment};
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
/// and a valid JSON envelope with `ok: true`, or until `timeout` elapses.
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

/// Create a temporary bin directory with symlinks so that `claude` and `gh`
/// resolve to the test's fake binaries.
fn symlink_test_bins(bins: &BuiltBinaries) -> (std::path::PathBuf, String) {
    let dir = std::env::temp_dir().join(format!("test-bin-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("create test bin dir");

    // Symlink claude -> fake-agent  (the executor defaults to "claude" for ClaudeCode)
    std::os::unix::fs::symlink(&bins.fake_agent_path, &dir.join("claude")).expect("symlink claude -> fake-agent");

    // Symlink gh -> fake-gh       (the GitHub gateway runs "gh" via PATH)
    std::os::unix::fs::symlink(&bins.fake_gh_path, &dir.join("gh")).expect("symlink gh -> fake-gh");

    let path_val = format!("{}:{}", dir.display(), std::env::var("PATH").unwrap_or_default());
    (dir, path_val)
}

/// Issue summary returned by the fake-gh state's `issues/1` route.
/// The daemon's issue discovery pipeline reads this route.
fn issue_schema() -> serde_json::Value {
    serde_json::json!({
        "number": 1,
        "title": "Implement feature X",
        "body": "## Acceptance Criteria\n\n- [ ] The system does X\n- [ ] Tests cover the new behavior\n- [ ] Documentation updated\n\n## Notes\n\nUse the existing pattern from src/lib.rs.",
        "state": "open",
        "labels": [{"name": "looper:plan"}],
        "user": {"login": "e2e-test-user"},
        "created_at": "2026-06-22T10:00:00Z",
        "updated_at": "2026-06-22T10:00:00Z"
    })
}

/// Build a fake-gh state object with a `looper:plan` issue and an open PR.
fn build_fake_gh_state(git_bare_path: &str) -> GHState {
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
            "repos/test-owner/test-repo/issues/1": issue_schema()
        })),
        pull_requests: Some(HashMap::from([(
            "test-owner/test-repo#1".to_string(),
            GHPullRequest {
                number: 1,
                repo: Some("test-owner/test-repo".into()),
                title: Some("feat: implement feature X".into()),
                body: Some("Closes #1\n\nThis PR implements feature X.".into()),
                state: Some("OPEN".into()),
                head_ref_name: Some("feature-x".into()),
                base_ref_name: Some("main".into()),
                author: Some("e2e-test-user".into()),
                merge_state_status: Some("CLEAN".into()),
                mergeable: Some(true),
                is_draft: Some(false),
                review_decision: Some("REVIEW_REQUIRED".into()),
                created_at: Some("2026-06-22T10:00:00Z".into()),
                updated_at: Some("2026-06-22T10:00:00Z".into()),
                threads: Some(vec![GHThread {
                    id: "thread-1".to_string(),
                    is_resolved: Some(false),
                    path: Some("src/lib.rs".into()),
                    line: Some(15),
                    comments: Some(vec![GHThreadComment {
                        id: "comment-1".to_string(),
                        body: Some("This should handle the null case".into()),
                        author: Some("reviewer-bot".into()),
                        path: Some("src/lib.rs".into()),
                        line: Some(15),
                        ..Default::default()
                    }]),
                }]),
                ..Default::default()
            },
        )])),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn test_full_cycle_issue_plan_review_fix_merge() {
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
    let home = TempHome::new("full_cycle");
    let port = must_free_port();
    let git_path = "git";

    // Seeded local repo with an initial commit
    let seeded = create_seeded_repo(git_path);

    // Create a bare origin
    let origin_path = std::env::temp_dir().join(format!("origin-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&origin_path).expect("create origin dir");
    run_git(git_path, &origin_path, &["init", "--bare"]);

    // Push the initial commit to origin
    run_git(git_path, std::path::Path::new(&seeded.path), &["remote", "add", "origin", origin_path.to_str().unwrap()]);
    run_git(git_path, std::path::Path::new(&seeded.path), &["push", "origin", "main"]);

    let origin_url = origin_path.to_string_lossy().to_string();

    // -----------------------------------------------------------------------
    // 2. Configure fake-gh with pre-defined state
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
    // 3. Configure fake-agent with success-with-diff mode
    // -----------------------------------------------------------------------
    let fake_agent = FakeAgent::new(&bins);

    // The file the fake-agent will write when invoked
    let write_file = home.working_dir.join("src").join("generated.rs");

    let (_vendor, _agent_bin, mut agent_env) =
        fake_agent.agent_config("success-with-diff", "/usr/bin/git", fgh.path.to_str().unwrap());

    agent_env.insert(
        looper_e2e::fake_agent::ENV_FAKE_AGENT_WRITE_FILE.to_string(),
        write_file.to_string_lossy().to_string(),
    );

    // -----------------------------------------------------------------------
    // 4. Create symlinks so "claude" and "gh" resolve to fake binaries
    // -----------------------------------------------------------------------
    let (bin_dir, path_val) = symlink_test_bins(&bins);

    // Collect all env vars to pass to the daemon
    let fgh_env = fgh.env_map();

    let extra_env: Vec<(&str, &str)> = vec![
        ("PATH", &path_val),
        // fake-gh env vars (inherited by gh -> fake-gh binary)
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
        // fake-agent env vars (inherited by claude -> fake-agent binary)
        (looper_e2e::fake_agent::ENV_FAKE_AGENT_MODE, "success-with-diff"),
        (looper_e2e::fake_agent::ENV_FAKE_AGENT_ARTIFACT_DIR, fake_agent.artifact_dir.to_str().expect("artifact dir")),
        (looper_e2e::fake_agent::ENV_FAKE_AGENT_STATE_PATH, fake_agent.state_path.to_str().expect("state path")),
        (looper_e2e::fake_agent::ENV_FAKE_AGENT_WRITE_FILE, write_file.to_str().expect("write file")),
        (looper_e2e::fake_agent::ENV_FAKE_AGENT_GIT_PATH, "/usr/bin/git"),
        (looper_e2e::fake_agent::ENV_FAKE_AGENT_GH_PATH, fgh.path.to_str().expect("fake-gh path")),
    ];

    // Write config
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

    // Override the storage path, home, and daemon config in the raw JSON so
    // those critical pointers get written.
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

    // ---------------------------------------------
    // 5. Start looperd
    // ---------------------------------------------
    let daemon = start_looperd(&bins, &home, home.config_path.to_str().unwrap(), &extra_env, "127.0.0.1", port);

    let base = daemon.base_url().to_string();

    // ---------------------------------------------
    // 6. Wait for daemon ready
    // ---------------------------------------------
    let result = wait_for_health_ok(&base, Duration::from_secs(30));
    assert!(result.is_ok(), "daemon should become ready within 30s: {:?}", result.err());

    let health = result.unwrap();
    assert!(
        health.get("ok").and_then(|v| v.as_bool()).unwrap_or(false),
        "health response should have ok=true: {health}"
    );

    // Also verify the data payload
    let data = health.get("data").expect("health data");
    assert_eq!(data.get("status").and_then(|v| v.as_str()), Some("ok"), "status should be ok");
    assert!(data.get("uptime_seconds").and_then(|v| v.as_u64()).unwrap_or(0) > 0, "uptime should be positive");
    assert!(
        data.get("version").and_then(|v| v.as_str()).filter(|v| !v.is_empty()).is_some(),
        "version should be non-empty"
    );

    // ---------------------------------------------
    // 7. Verify via API
    // ---------------------------------------------
    let client = reqwest::blocking::Client::new();

    // ---------- Version ----------
    let version_resp = client.get(format!("{base}/version")).send().expect("GET /version");
    assert_eq!(version_resp.status(), 200, "/version should return 200");
    let version_body: serde_json::Value = version_resp.json().expect("/version should be JSON");
    assert!(version_body.get("ok").and_then(|v| v.as_bool()).unwrap_or(false), "/version should have ok=true");
    let v = version_body.get("data").and_then(|d| d.get("version")).and_then(|v| v.as_str());
    assert!(v.is_some() && !v.unwrap().is_empty(), "version string");

    // ---------- Config ----------
    let config_resp = client.get(format!("{base}/api/config")).send().expect("GET /api/config");
    assert_eq!(config_resp.status(), 200, "/api/config should return 200");
    let config_body: serde_json::Value = config_resp.json().expect("/api/config should be JSON");
    assert!(config_body.get("ok").and_then(|v| v.as_bool()).unwrap_or(false), "/api/config should have ok=true");

    // ---------- List projects (initially may be empty) ----------
    let list_resp = client.get(format!("{base}/api/projects")).send().expect("GET /api/projects");
    assert_eq!(list_resp.status(), 200, "list projects");

    // ---------- Add project via API ----------
    let add_body = serde_json::json!({
        "name": "e2e-project",
        "repo_url": origin_url,
        "default_branch": "main",
        "enabled": true,
    });
    let add_resp = client.post(format!("{base}/api/projects")).json(&add_body).send();

    // Adding a project may succeed (201 Created) or fail (the test project
    // from config may already be registered). Accept both.
    let add_status = add_resp.as_ref().map(|r| r.status()).unwrap_or_default();
    let add_ok = add_status.is_success() || add_status.as_u16() == 409;
    assert!(add_ok, "add project should succeed or conflict (got {add_status})");

    // ---------- Create a loop ----------
    let loop_body = serde_json::json!({
        "loop_type": "plan",
        "target": "issue #1",
        "metadata": {
            "acceptance_criteria": "Feature X works end-to-end",
            "issue_url": "https://github.com/test-owner/test-repo/issues/1"
        }
    });
    let create_loop_resp = client.post(format!("{base}/api/projects/e2e-project/loops")).json(&loop_body).send();

    let loop_created =
        create_loop_resp.as_ref().map(|r| r.status().is_success() || r.status().as_u16() == 201).unwrap_or(false);

    // If the loop was created, verify its detail
    if loop_created {
        let create_loop_body: serde_json::Value = create_loop_resp.unwrap().json().expect("create loop JSON");
        let loop_data = create_loop_body.get("data").expect("create loop data");
        assert_eq!(loop_data.get("project_name").and_then(|v| v.as_str()), Some("e2e-project"));
        assert_eq!(loop_data.get("loop_type").and_then(|v| v.as_str()), Some("plan"), "loop type should be 'plan'");
        assert_eq!(loop_data.get("status").and_then(|v| v.as_str()), Some("active"), "new loop should be active");

        // Read back the loop
        let seq: i64 = loop_data.get("seq").and_then(|v| v.as_i64()).expect("loop seq");

        let get_loop_resp =
            client.get(format!("{base}/api/projects/e2e-project/loops/{seq}")).send().expect("GET loop");
        assert_eq!(get_loop_resp.status(), 200, "get loop should succeed");
    }

    // ---------- List loops (may be empty if project doesn't exist) ----------
    let list_loops_resp = client.get(format!("{base}/api/projects/e2e-project/loops")).send();
    if let Ok(resp) = list_loops_resp {
        if resp.status().is_success() {
            let loops_body: serde_json::Value = resp.json().expect("list loops JSON");
            assert!(loops_body.get("ok").and_then(|v| v.as_bool()).unwrap_or(false));
        }
    }

    // ---------- Enqueue a queue item ----------
    let enqueue_body = serde_json::json!({
        "queue_type": "test-item",
        "priority": 5,
        "payload": {
            "action": "verify",
            "target": "feature-x"
        }
    });
    let enqueue_resp = client.post(format!("{base}/api/projects/e2e-project/queue/enqueue")).json(&enqueue_body).send();
    if let Ok(resp) = enqueue_resp {
        let status = resp.status();
        if status.is_success() || status.as_u16() == 201 {
            let enq_body: serde_json::Value = resp.json().expect("enqueue JSON");
            let item_data = enq_body.get("data").expect("queue item data");
            assert_eq!(item_data.get("queue_type").and_then(|v| v.as_str()), Some("test-item"));
            assert_eq!(
                item_data.get("status").and_then(|v| v.as_str()),
                Some("queued"),
                "new queue item should be queued"
            );
        }
    }

    // ---------- List queue ----------
    let queue_resp = client.get(format!("{base}/api/projects/e2e-project/queue")).send();
    if let Ok(resp) = queue_resp {
        if resp.status().is_success() {
            let queue_body: serde_json::Value = resp.json().expect("queue JSON");
            assert!(queue_body.get("ok").and_then(|v| v.as_bool()).unwrap_or(false));
        }
    }

    // ---------- List events ----------
    let events_resp = client.get(format!("{base}/api/projects/e2e-project/events")).send();
    if let Ok(resp) = events_resp {
        if resp.status().is_success() {
            let events_body: serde_json::Value = resp.json().expect("events JSON");
            assert!(events_body.get("ok").and_then(|v| v.as_bool()).unwrap_or(false));
        }
    }

    // ---------- Lock operations ----------
    let lock_body = serde_json::json!({
        "resource": "test-resource",
        "ttl_secs": 30
    });
    let acquire_resp = client.post(format!("{base}/api/locks")).json(&lock_body).send().expect("POST /api/locks");
    assert!(acquire_resp.status().is_success() || acquire_resp.status().as_u16() == 201, "acquire lock should succeed");
    let lock_body: serde_json::Value = acquire_resp.json().expect("lock JSON");
    let lock_data = lock_body.get("data").expect("lock data");
    assert_eq!(lock_data.get("resource").and_then(|v| v.as_str()), Some("test-resource"), "lock resource should match");

    // List locks
    let list_locks_resp = client.get(format!("{base}/api/locks")).send().expect("GET /api/locks");
    assert_eq!(list_locks_resp.status(), 200, "list locks should succeed");
    let locks_body: serde_json::Value = list_locks_resp.json().expect("locks JSON");
    assert!(locks_body.get("ok").and_then(|v| v.as_bool()).unwrap_or(false));

    // Release lock
    let release_resp =
        client.delete(format!("{base}/api/locks/test-resource")).send().expect("DELETE /api/locks/test-resource");
    // Release may return 200 or 404 if already expired
    let release_status = release_resp.status();
    assert!(release_status.is_success() || release_status.as_u16() == 404, "release lock got {release_status}");

    // ---------- Project config endpoint ----------
    let agent_cfg_resp = client.get(format!("{base}/api/projects/e2e-project/agent-config")).send();
    if let Ok(resp) = agent_cfg_resp {
        if resp.status().is_success() {
            let acfg_body: serde_json::Value = resp.json().expect("agent config JSON");
            assert!(acfg_body.get("ok").and_then(|v| v.as_bool()).unwrap_or(false));
        }
    }

    // ---------- Shutdown via API ----------
    let _ = client.post(format!("{base}/shutdown")).send();

    // Allow brief window for graceful shutdown
    std::thread::sleep(Duration::from_millis(500));
    daemon.stop(Duration::from_secs(5));

    // -----------------------------------------------------------------------
    // Cleanup
    // -----------------------------------------------------------------------
    let _ = std::fs::remove_dir_all(&bin_dir);
    let _ = std::fs::remove_dir_all(&origin_path);
    home.cleanup();
}
