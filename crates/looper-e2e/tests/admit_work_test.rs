//! E2E: POST /api/projects/{name}/work creates claimable queue + progresses.
//!
//! Skipped when E2E binary env vars are not set (plain `cargo test`).

use std::collections::HashMap;
use std::time::Duration;

use looper_e2e::binaries::BuiltBinaries;
use looper_e2e::config::{default_config, write_config, ConfigOptions};
use looper_e2e::daemon::start_looperd;
use looper_e2e::ports::must_free_port;
use looper_e2e::temp_home::TempHome;

fn require_bins() -> Option<BuiltBinaries> {
    BuiltBinaries::from_env()
}

#[test]
fn test_admit_work_creates_queue_item() {
    let bins = match require_bins() {
        Some(b) => b,
        None => {
            eprintln!("SKIP: E2E binary env vars not set");
            return;
        }
    };

    let home = TempHome::new("admit_work");
    let port = must_free_port();
    let cfg = default_config(&home, ConfigOptions { port: Some(port), ..Default::default() });
    write_config(&home.config_path, &cfg, HashMap::new());

    let daemon = start_looperd(&bins, &home, home.config_path.to_str().unwrap(), &[], "127.0.0.1", port);
    let ready = daemon.wait_for_ready(Duration::from_secs(20));
    assert!(ready.is_ok(), "daemon ready: {:?}", ready.err());

    let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(10)).build().expect("http client");
    let base = daemon.base_url();

    // Add project with resolvable GitHub repo (B8 contract).
    let add_url = format!("{base}/api/projects");
    let add_body = serde_json::json!({
        "name": "demo",
        "path": home.working_dir.to_string_lossy(),
        "repo_url": "acme/demo",
        "default_branch": "main",
        "enabled": true
    });
    let add_resp = client.post(&add_url).json(&add_body).send().expect("add project");
    let add_status = add_resp.status();
    let add_json: serde_json::Value = add_resp.json().unwrap_or_default();
    eprintln!("add_project status={add_status} body={add_json}");
    assert!(add_status.is_success(), "add project should succeed: {add_json}");

    // Admit planner work on issue #42.
    let work_url = format!("{base}/api/projects/demo/work");
    let work_body = serde_json::json!({
        "role": "planner",
        "issue_number": 42
    });
    let work_resp = client.post(&work_url).json(&work_body).send().expect("admit work");
    let work_status = work_resp.status();
    let work_json: serde_json::Value = work_resp.json().expect("admit json");
    eprintln!("admit_work status={work_status} project=demo role=planner issue=42 body={work_json}");
    assert!(work_status.is_success(), "admit work should succeed: {work_json}");

    let data = work_json.get("data").expect("data");
    let queue_type = data.pointer("/queue_item/queue_type").and_then(|v| v.as_str()).unwrap_or("");
    let queue_status = data.pointer("/queue_item/status").and_then(|v| v.as_str()).unwrap_or("");
    let queue_id = data.pointer("/queue_item/id").and_then(|v| v.as_str()).unwrap_or("");
    let loop_seq = data.pointer("/loop_detail/seq").and_then(|v| v.as_i64()).unwrap_or(-1);
    let tick = data.get("tick_triggered").and_then(|v| v.as_bool()).unwrap_or(false);

    eprintln!(
        "admit_work ok queue_id={queue_id} type={queue_type} status={queue_status} loop_seq={loop_seq} tick={tick}"
    );
    assert_eq!(queue_type, "planner");
    assert!(
        queue_status == "queued" || queue_status == "running" || queue_status == "completed",
        "queue should be claimable or progressing, got {queue_status}"
    );
    assert!(tick, "tick_triggered should be true");
    assert!(!queue_id.is_empty());

    // List queue — item visible.
    let q_url = format!("{base}/api/projects/demo/queue?limit=50&offset=0");
    let q_resp = client.get(&q_url).send().expect("list queue");
    let q_json: serde_json::Value = q_resp.json().expect("queue json");
    eprintln!("list_queue body={q_json}");
    let items = q_json.get("data").and_then(|d| d.as_array()).cloned().unwrap_or_default();
    assert!(
        items.iter().any(|i| i.get("id").and_then(|v| v.as_str()) == Some(queue_id)),
        "queue list should contain admitted item {queue_id}: {items:?}"
    );

    // Negative: project without repo.
    let bad_add = serde_json::json!({
        "name": "norepo",
        "path": "/tmp/looper-e2e-norepo-does-not-need-exist",
        "enabled": true
    });
    let _ = client.post(&add_url).json(&bad_add).send();
    let bad_work = client
        .post(format!("{base}/api/projects/norepo/work"))
        .json(&serde_json::json!({"role":"planner","issue_number":1}))
        .send()
        .expect("bad admit");
    let bad_status = bad_work.status();
    let bad_json: serde_json::Value = bad_work.json().unwrap_or_default();
    eprintln!("admit without repo status={bad_status} body={bad_json}");
    // 4xx expected when repo unresolved (or project path invalid).
    assert!(
        bad_status.is_client_error() || !bad_json.get("ok").and_then(|v| v.as_bool()).unwrap_or(true),
        "missing repo should fail closed: {bad_json}"
    );

    daemon.stop(Duration::from_secs(5));
}
