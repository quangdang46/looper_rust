//! Autoupgrade: version check against the real release repo, manual upgrade path.
//!
//! Automated binary download/swap is intentionally **not** implemented here.
//! Use the install script or `looper daemon install` (see [`manual_upgrade_instructions`]).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::CliError;

/// GitHub owner/repo that publishes Looper releases.
///
/// Must match daemon install (`daemon.rs`) and README badges:
/// `https://github.com/quangdang46/looper/releases`.
pub const RELEASE_REPO: &str = "quangdang46/looper";

/// State file for tracking auto-upgrade checks.
const STATE_FILE: &str = "autoupgrade-state.json";

/// Lock file to prevent concurrent upgrade attempts (reserved for future auto path).
const LOCK_FILE: &str = "autoupgrade.lock";

#[derive(Debug, Serialize, Deserialize)]
pub struct AutoupgradeState {
    pub last_checked: String,
    pub current_version: String,
    pub available_version: Option<String>,
    pub download_url: Option<String>,
}

/// Path to the state file.
fn state_path() -> Result<PathBuf, CliError> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    Ok(PathBuf::from(home).join(".local").join("share").join("looper").join(STATE_FILE))
}

/// Path to the lock file.
#[allow(dead_code)]
fn lock_path() -> Result<PathBuf, CliError> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    Ok(PathBuf::from(home).join(".local").join("share").join("looper").join(LOCK_FILE))
}

/// GitHub API URL for the latest release of [`RELEASE_REPO`].
#[must_use]
pub fn latest_release_api_url() -> String {
    format!("https://api.github.com/repos/{RELEASE_REPO}/releases/latest")
}

/// Rust target triple for the current host, if supported for releases.
#[must_use]
pub fn platform_triple() -> Option<&'static str> {
    platform_triple_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// Map OS/arch to a release target triple.
#[must_use]
pub fn platform_triple_for(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        _ => None,
    }
}

/// Release asset filename for a binary on a given triple (daemon install convention).
#[must_use]
pub fn asset_name_for_triple(binary: &str, triple: &str) -> String {
    format!("{binary}-{triple}.tar.gz")
}

/// Manual upgrade instructions (no automated download).
#[must_use]
pub fn manual_upgrade_instructions(available_version: Option<&str>) -> String {
    let version_hint = available_version.unwrap_or("latest");
    format!(
        "Automatic binary download is not implemented.\n\
         \n\
         Upgrade manually using one of:\n\
         \n\
         1) Install script (CLI + daemon):\n\
            curl -fsSL https://github.com/{RELEASE_REPO}/releases/latest/download/install.sh | bash\n\
         \n\
         2) Daemon-only version pin:\n\
            looper daemon install --version {version_hint}\n\
         \n\
         Releases: https://github.com/{RELEASE_REPO}/releases\n\
         After installing, restart the daemon if it is running."
    )
}

/// Read the current state, or return a default.
pub fn read_state() -> Result<AutoupgradeState, CliError> {
    let path = state_path()?;
    if !path.exists() {
        return Ok(AutoupgradeState {
            last_checked: String::new(),
            current_version: crate::version::VERSION.to_string(),
            available_version: None,
            download_url: None,
        });
    }
    let raw = std::fs::read_to_string(&path)?;
    serde_json::from_str(&raw).map_err(CliError::from)
}

/// Write state.
pub fn write_state(state: &AutoupgradeState) -> Result<(), CliError> {
    let path = state_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let raw = serde_json::to_string_pretty(state)?;
    std::fs::write(&path, &raw)?;
    Ok(())
}

/// Check for a new version by querying GitHub releases on [`RELEASE_REPO`].
pub async fn check() -> Result<(), CliError> {
    check_at_url(&latest_release_api_url()).await
}

/// Check using an explicit releases API URL (used by tests with a mock server).
pub async fn check_at_url(api_url: &str) -> Result<(), CliError> {
    let state = read_state()?;
    println!("Current version: {}", state.current_version);
    println!("Release repo: {RELEASE_REPO}");

    let client = reqwest::Client::builder()
        .user_agent("looper-cli")
        .build()
        .map_err(|e| CliError::autoupgrade(format!("HTTP client error: {e}")))?;

    let resp = client
        .get(api_url)
        .send()
        .await
        .map_err(|e| CliError::autoupgrade(format!("cannot check for updates: {e}")))?;

    if !resp.status().is_success() {
        return Err(CliError::autoupgrade(format!("GitHub API returned {}", resp.status())));
    }

    let release: serde_json::Value =
        resp.json().await.map_err(|e| CliError::autoupgrade(format!("cannot parse release info: {e}")))?;

    let tag = release["tag_name"].as_str().unwrap_or("unknown");
    let html_url = release["html_url"].as_str().unwrap_or("");

    let cur_ver = state.current_version.clone();
    let new_state = AutoupgradeState {
        last_checked: chrono::Utc::now().to_rfc3339(),
        current_version: cur_ver.clone(),
        available_version: Some(tag.to_string()),
        download_url: Some(html_url.to_string()),
    };
    write_state(&new_state)?;

    println!("Latest release: {tag}");
    if tag != cur_ver && !tag.contains(&cur_ver) {
        println!("A new version is available: {tag}");
        if !html_url.is_empty() {
            println!("Release page: {html_url}");
        }
        println!();
        println!("{}", manual_upgrade_instructions(Some(tag)));
    } else {
        println!("You are on the latest version.");
    }
    Ok(())
}

/// Show the current upgrade state.
pub async fn status() -> Result<(), CliError> {
    let state = read_state()?;
    println!("Current version: {}", state.current_version);
    println!("Release repo: {RELEASE_REPO}");
    if let Some(avail) = &state.available_version {
        println!("Available version: {avail}");
    } else {
        println!("No version check performed yet. Run `looper autoupgrade check`.");
    }
    if let Some(url) = &state.download_url {
        println!("Release page: {url}");
    }
    if !state.last_checked.is_empty() {
        println!("Last checked: {}", state.last_checked);
    }
    println!();
    println!("Note: automated download is not implemented. Use manual install instructions.");
    Ok(())
}

/// Print manual upgrade instructions only — does **not** download or swap binaries.
pub async fn upgrade() -> Result<(), CliError> {
    let state = read_state()?;
    let version = state.available_version.as_deref();

    if version.is_none() {
        println!("No available version known yet (run `looper autoupgrade check` first).");
        println!();
    } else if let Some(v) = version {
        println!("Target version from last check: {v}");
        println!();
    }

    // Demoted path: never claim automated download succeeded.
    println!("{}", manual_upgrade_instructions(version));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;

    #[test]
    fn release_repo_is_quangdang46_looper() {
        assert_eq!(RELEASE_REPO, "quangdang46/looper");
        assert!(!RELEASE_REPO.contains("looper-ai"));
    }

    #[test]
    fn latest_release_api_url_targets_correct_repo() {
        let url = latest_release_api_url();
        assert_eq!(url, "https://api.github.com/repos/quangdang46/looper/releases/latest");
        assert!(url.contains("api.github.com"));
        assert!(url.contains("/repos/quangdang46/looper/"));
        assert!(!url.contains("looper-ai"));
    }

    #[test]
    fn asset_name_for_target_triples() {
        assert_eq!(
            asset_name_for_triple("looperd", "x86_64-unknown-linux-gnu"),
            "looperd-x86_64-unknown-linux-gnu.tar.gz"
        );
        assert_eq!(asset_name_for_triple("looperd", "aarch64-apple-darwin"), "looperd-aarch64-apple-darwin.tar.gz");
        assert_eq!(asset_name_for_triple("looper-cli", "x86_64-apple-darwin"), "looper-cli-x86_64-apple-darwin.tar.gz");
    }

    #[test]
    fn platform_triple_for_known_hosts() {
        assert_eq!(platform_triple_for("linux", "x86_64"), Some("x86_64-unknown-linux-gnu"));
        assert_eq!(platform_triple_for("macos", "aarch64"), Some("aarch64-apple-darwin"));
        assert_eq!(platform_triple_for("windows", "x86_64"), None);
    }

    #[test]
    fn manual_instructions_do_not_claim_auto_download() {
        let msg = manual_upgrade_instructions(Some("v1.2.3"));
        assert!(msg.contains("not implemented") || msg.contains("manually"));
        assert!(msg.contains(RELEASE_REPO));
        assert!(msg.contains("install.sh") || msg.contains("daemon install"));
        assert!(!msg.to_lowercase().contains("download complete"));
        assert!(!msg.to_lowercase().contains("upgrade successful"));
        assert!(!msg.to_lowercase().contains("swapped binary"));
    }

    #[tokio::test]
    async fn upgrade_never_ok_claims_download() {
        // upgrade always returns Ok after printing manual instructions only.
        let result = upgrade().await;
        assert!(result.is_ok(), "manual upgrade path should succeed without claiming download: {result:?}");
        // Ensure the message content used by upgrade is manual-only.
        let msg = manual_upgrade_instructions(None);
        assert!(msg.contains("Automatic binary download is not implemented"));
    }

    #[tokio::test]
    async fn check_hits_expected_repo_path_via_mock_http() {
        // Tiny mock GitHub API that records the request path.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let addr = listener.local_addr().unwrap();
        let seen_path: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let seen_path_bg = Arc::clone(&seen_path);

        let body = r#"{"tag_name":"v9.9.9","html_url":"https://github.com/quangdang46/looper/releases/tag/v9.9.9"}"#;
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 2048];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                // First line: GET /repos/quangdang46/looper/releases/latest HTTP/1.1
                if let Some(line) = req.lines().next() {
                    let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                    *seen_path_bg.lock().unwrap() = path;
                }
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });

        // Isolate state file under a temp HOME.
        let tmp = tempfile_dir();
        let prev_home = std::env::var("HOME").ok();
        // SAFETY: test-only, single-threaded control of HOME for this process.
        unsafe {
            std::env::set_var("HOME", &tmp);
        }

        let api_url = format!("http://{addr}/repos/{RELEASE_REPO}/releases/latest");
        let result = check_at_url(&api_url).await;

        if let Some(h) = prev_home {
            unsafe {
                std::env::set_var("HOME", h);
            }
        } else {
            unsafe {
                std::env::remove_var("HOME");
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);

        result.expect("check against mock should succeed");
        let path = seen_path.lock().unwrap().clone();
        assert_eq!(
            path,
            format!("/repos/{RELEASE_REPO}/releases/latest"),
            "check must hit release repo path, got {path}"
        );
        assert!(path.contains("quangdang46/looper"));
        assert!(!path.contains("looper-ai"));
    }

    fn tempfile_dir() -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("looper-autoupgrade-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
