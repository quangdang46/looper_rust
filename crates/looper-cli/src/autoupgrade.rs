use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::CliError;

/// State file for tracking auto-upgrade.
const STATE_FILE: &str = "autoupgrade-state.json";

/// Lock file to prevent concurrent upgrades.
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
fn lock_path() -> Result<PathBuf, CliError> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    Ok(PathBuf::from(home).join(".local").join("share").join("looper").join(LOCK_FILE))
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

/// Check for a new version by querying GitHub releases.
pub async fn check() -> Result<(), CliError> {
    let state = read_state()?;
    println!("Current version: {}", state.current_version);

    // Fetch latest release from GitHub
    let url = "https://api.github.com/repos/looper-ai/looper/releases/latest";
    let client = reqwest::Client::builder()
        .user_agent("looper-cli")
        .build()
        .map_err(|e| CliError::autoupgrade(format!("HTTP client error: {e}")))?;

    let resp = client.get(url).send().await.map_err(|e| {
        CliError::autoupgrade(format!("cannot check for updates: {e}"))
    })?;

    if !resp.status().is_success() {
        return Err(CliError::autoupgrade(
            format!("GitHub API returned {}", resp.status()),
        ));
    }

    let release: serde_json::Value = resp.json().await.map_err(|e| {
        CliError::autoupgrade(format!("cannot parse release info: {e}"))
    })?;

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
        println!("Download: {html_url}");
    } else {
        println!("You are on the latest version.");
    }
    Ok(())
}

/// Show the current upgrade state.
pub async fn status() -> Result<(), CliError> {
    let state = read_state()?;
    println!("Current version: {}", state.current_version);
    if let Some(avail) = &state.available_version {
        println!("Available version: {avail}");
    } else {
        println!("No version check performed yet. Run `looper autoupgrade check`.");
    }
    if let Some(url) = &state.download_url {
        println!("Download: {url}");
    }
    if !state.last_checked.is_empty() {
        println!("Last checked: {}", state.last_checked);
    }
    Ok(())
}

/// Perform the upgrade: download the release binary and swap.
#[allow(dead_code)]
pub async fn upgrade() -> Result<(), CliError> {
    let state = read_state()?;
    let Some(version) = &state.available_version else {
        return Err(CliError::autoupgrade(
            "no available version known; run `looper autoupgrade check` first",
        ));
    };
    let Some(url) = &state.download_url else {
        return Err(CliError::autoupgrade("no download URL available"));
    };

    // Acquire lock
    let lock = lock_path()?;
    if lock.exists() {
        return Err(CliError::autoupgrade(
            "upgrade lock exists — another upgrade may be in progress",
        ));
    }
    std::fs::write(&lock, "")?;

    let result = perform_download_and_swap(version, url).await;
    let _ = std::fs::remove_file(&lock);
    result
}

async fn perform_download_and_swap(_version: &str, url: &str) -> Result<(), CliError> {
    let current_exe = std::env::current_exe()
        .map_err(|e| CliError::autoupgrade(format!("cannot determine current binary: {e}")))?;

    // Build asset URL (assuming GitHub release tar.gz or binary)
    // For simplicity, report the download URL — actual download is platform-specific
    println!("Upgrade requires manual download from: {url}");
    println!("Replace binary at: {}", current_exe.display());

    // Placeholder: in production, detect platform → construct asset URL → download → verify → swap
    Err(CliError::autoupgrade(
        "automatic download not yet implemented; download the release manually",
    ))
}
