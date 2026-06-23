//! Smoke test: verify that `looperd` starts, serves its status endpoint,
//! and shuts down cleanly.
//!
//! Ported from Go `legacy/internal/e2e/smoke_daemon_test.go`.
//!
//! These tests require E2E binary paths set via environment variables.
//! They are skipped when the variables are not set (e.g. during `cargo test`).

use std::collections::HashMap;
use std::time::Duration;

use looper_e2e::*;
use looper_e2e::daemon::start_looperd;
use looper_e2e::temp_home::TempHome;
use looper_e2e::binaries::BuiltBinaries;
use looper_e2e::ports::must_free_port;
use looper_e2e::config::{default_config, write_config, ConfigOptions};

fn require_bins() -> Option<BuiltBinaries> {
    BuiltBinaries::from_env()
}

#[test]
fn test_daemon_starts_and_serves_health() {
    let bins = match require_bins() {
        Some(b) => b,
        None => {
            eprintln!("SKIP: E2E binary env vars not set");
            return;
        }
    };

    let home = TempHome::new("daemon_health");
    let port = must_free_port();
    let cfg = default_config(
        &home,
        ConfigOptions {
            port: Some(port),
            ..Default::default()
        },
    );
    write_config(&home.config_path, &cfg, HashMap::new());

    let daemon = start_looperd(
        &bins,
        &home,
        home.config_path.to_str().unwrap(),
        &[],
        "127.0.0.1",
        port,
    );

    let result = daemon.wait_for_ready(Duration::from_secs(15));
    assert!(
        result.is_ok(),
        "daemon should become ready within 15s: {:?}",
        result.err()
    );

    let body = result.unwrap();
    assert!(
        body.get("ok").and_then(|v| v.as_bool()).unwrap_or(false),
        "status response should have ok=true: {body}"
    );

    daemon.stop(Duration::from_secs(5));
}

#[test]
fn test_daemon_shows_version() {
    let bins = match require_bins() {
        Some(b) => b,
        None => {
            eprintln!("SKIP: E2E binary env vars not set");
            return;
        }
    };

    let home = TempHome::new("daemon_version");
    let port = must_free_port();
    let cfg = default_config(
        &home,
        ConfigOptions {
            port: Some(port),
            ..Default::default()
        },
    );
    write_config(&home.config_path, &cfg, HashMap::new());

    let daemon = start_looperd(
        &bins,
        &home,
        home.config_path.to_str().unwrap(),
        &[],
        "127.0.0.1",
        port,
    );

    let result = daemon.wait_for_ready(Duration::from_secs(15));
    assert!(result.is_ok(), "daemon should become ready");

    let client = reqwest::blocking::Client::new();
    let version_url = format!("{}/version", daemon.base_url());
    let resp = client
        .get(&version_url)
        .send()
        .expect("should get version endpoint");
    assert_eq!(resp.status(), 200, "version endpoint should return 200");

    let body: serde_json::Value = resp.json().expect("version should be JSON");
    assert!(
        body.get("data")
            .and_then(|d| d.get("version"))
            .and_then(|v| v.as_str())
            .is_some(),
        "version response should contain version string: {body}"
    );

    daemon.stop(Duration::from_secs(5));
}
