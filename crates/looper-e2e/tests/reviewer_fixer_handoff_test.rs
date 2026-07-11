//! E2E: fixer queue admission + dedupe (reviewer→fixer convergence surface).
//!
//! Full agent review publish is covered by unit tests in `looper-runner`
//! (`should_enqueue_fixer_table`, `ensure_fixer_queue_item_*`). This test
//! proves the daemon admits fixer work for a PR (same queue shape C1 creates)
//! and dedupes re-admit — the claimable end of the handoff.
//!
//! Skipped when E2E binary env vars are not set.

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
fn test_fixer_admit_and_dedupe_for_pr() {
    let bins = match require_bins() {
        Some(b) => b,
        None => {
            eprintln!("SKIP: E2E binary env vars not set");
            return;
        }
    };

    let home = TempHome::new("reviewer_fixer_handoff");
    let port = must_free_port();
    let cfg = default_config(&home, ConfigOptions { port: Some(port), ..Default::default() });
    write_config(&home.config_path, &cfg, HashMap::new());

    let daemon = start_looperd(&bins, &home, home.config_path.to_str().unwrap(), &[], "127.0.0.1", port);
    assert!(daemon.wait_for_ready(Duration::from_secs(20)).is_ok());

    let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(10)).build().expect("client");
    let base = daemon.base_url();

    let add = client
        .post(format!("{base}/api/projects"))
        .json(&serde_json::json!({
            "name": "handoff",
            "path": home.working_dir.to_string_lossy(),
            "repo_url": "acme/handoff",
            "default_branch": "main",
            "enabled": true
        }))
        .send()
        .expect("add");
    assert!(add.status().is_success(), "add project: {:?}", add.text());

    let pr_number = 99i64;
    eprintln!("pr_number={pr_number} review_decision=CHANGES_REQUESTED (simulated via admit fixer)");

    let work_url = format!("{base}/api/projects/handoff/work");
    let body = serde_json::json!({
        "role": "fixer",
        "pr_number": pr_number
    });

    let r1 = client.post(&work_url).json(&body).send().expect("admit1");
    let j1: serde_json::Value = r1.json().expect("json1");
    eprintln!("first admit body={j1}");
    assert_eq!(j1["ok"], true);
    let qid1 = j1["data"]["queue_item"]["id"].as_str().unwrap_or("").to_string();
    let qtype = j1["data"]["queue_item"]["queue_type"].as_str().unwrap_or("");
    let status = j1["data"]["queue_item"]["status"].as_str().unwrap_or("");
    eprintln!("enqueue queue_id={qid1} type={qtype} status={status}");
    assert_eq!(qtype, "fixer");
    assert!(!qid1.is_empty());
    assert!(status == "queued" || status == "running");

    // Dedupe re-admit same PR
    let r2 = client.post(&work_url).json(&body).send().expect("admit2");
    let j2: serde_json::Value = r2.json().expect("json2");
    let qid2 = j2["data"]["queue_item"]["id"].as_str().unwrap_or("");
    let created_new = j2["data"]["created_new_loop"].as_bool().unwrap_or(true);
    eprintln!("second admit queue_id={qid2} created_new_loop={created_new}");
    assert_eq!(qid2, qid1, "dedupe should return same queue item");
    assert!(!created_new, "should reuse loop");

    // Reviewer admit for same PR is separate role (not fixer) — both may exist.
    let rev = client
        .post(&work_url)
        .json(&serde_json::json!({"role":"reviewer","pr_number": pr_number}))
        .send()
        .expect("admit reviewer");
    let jr: serde_json::Value = rev.json().expect("json r");
    eprintln!("reviewer admit body={jr}");
    assert_eq!(jr["ok"], true);
    assert_eq!(jr["data"]["queue_item"]["queue_type"], "reviewer");

    daemon.stop(Duration::from_secs(5));
}
