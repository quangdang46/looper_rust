//! Webhook tunnel server: manage GitHub webhooks via gh CLI.
//!
//! Ported from Go `legacy/internal/runtime/webhook_tunnel.go`.
//! Creates/updates/deletes GitHub webhooks via gh CLI API, HMAC auth,
//! disable latch (auto-off after N failures), and forwarder exit classification.

use hmac::{Hmac, Mac};
use sha2::Sha256;

use looper_storage::repos::Repositories;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const DISABLE_LATCH_THRESHOLD: usize = 3;

// ---------------------------------------------------------------------------
// GitHub webhook client
// ---------------------------------------------------------------------------

/// A GitHub webhook as returned by the gh API.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GitHubHook {
    pub id: i64,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub config: HookConfig,
    #[serde(default)]
    pub last_response: Option<HookLastResponse>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HookConfig {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub content_type: String,
    #[serde(default)]
    pub insecure_ssl: String,
}

impl Default for HookConfig {
    fn default() -> Self {
        Self { url: String::new(), content_type: "json".into(), insecure_ssl: "0".into() }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HookLastResponse {
    pub code: Option<i32>,
}

/// Client for managing GitHub webhooks via gh CLI.
#[derive(Debug, Clone)]
pub struct GhWebhookClient {
    pub gh_path: String,
}

impl GhWebhookClient {
    pub fn new(gh_path: &str) -> Self {
        Self { gh_path: gh_path.to_string() }
    }

    fn repo_path(repo: &str) -> String {
        repo.trim().to_lowercase()
    }

    pub fn get_hook(&self, repo: &str, id: i64) -> Result<Option<GitHubHook>, String> {
        let rp = Self::repo_path(repo);
        match run_gh(&self.gh_path, &["api", &format!("repos/{rp}/hooks/{id}")]) {
            Ok(output) if !output.is_empty() => {
                serde_json::from_str(&output).map(Some).map_err(|e| format!("decode hook: {e}"))
            }
            Ok(_) => Ok(None),
            Err(e) => {
                if e.contains("404") || e.contains("Not Found") {
                    Ok(None)
                } else {
                    Err(e)
                }
            }
        }
    }

    pub fn create_hook(&self, repo: &str, hook_url: &str, secret: &str, events: &[&str]) -> Result<GitHubHook, String> {
        let body = build_hook_body(hook_url, secret, events, true);
        let tmp = write_tmp_json(&body)?;
        let rp = Self::repo_path(repo);
        let output = run_gh(&self.gh_path, &["api", "-X", "POST", &format!("repos/{rp}/hooks"), "--input", &tmp])?;
        let _ = std::fs::remove_file(&tmp);
        serde_json::from_str(&output).map_err(|e| format!("decode created hook: {e}"))
    }

    pub fn update_hook(
        &self,
        repo: &str,
        id: i64,
        hook_url: &str,
        secret: &str,
        events: &[&str],
        active: bool,
    ) -> Result<GitHubHook, String> {
        let body = build_hook_body(hook_url, secret, events, active);
        let tmp = write_tmp_json(&body)?;
        let rp = Self::repo_path(repo);
        let output =
            run_gh(&self.gh_path, &["api", "-X", "PATCH", &format!("repos/{rp}/hooks/{id}"), "--input", &tmp])?;
        let _ = std::fs::remove_file(&tmp);
        serde_json::from_str(&output).map_err(|e| format!("decode updated hook: {e}"))
    }

    pub fn delete_hook(&self, repo: &str, id: i64) -> Result<(), String> {
        let rp = Self::repo_path(repo);
        run_gh(&self.gh_path, &["api", "-X", "DELETE", &format!("repos/{rp}/hooks/{id}")]).map(|_| ())
    }
}

// ---------------------------------------------------------------------------
// Tunnel secret
// ---------------------------------------------------------------------------

/// Derive an HMAC-SHA256 secret for webhook verification.
pub fn derive_tunnel_secret(daemon_token: &str, repo: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(daemon_token.as_bytes()).expect("HMAC key");
    mac.update(repo.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

// ---------------------------------------------------------------------------
// Disable latch
// ---------------------------------------------------------------------------

/// Check if tunnel should be auto-disabled after N failures.
pub fn should_disable_tunnel(tunnel_id: &str, repos: &Repositories) -> Result<bool, String> {
    let events = repos.events.list(100).map_err(|e| format!("list tunnel events: {e}"))?;
    let failures = events
        .iter()
        .filter(|e| {
            e.entity_type.as_deref() == Some("tunnel")
                && e.entity_id.as_deref() == Some(tunnel_id)
                && e.event_type == "webhook_delivery_failed"
        })
        .count();
    Ok(failures >= DISABLE_LATCH_THRESHOLD)
}

// ---------------------------------------------------------------------------
// Forwarder exit classification
// ---------------------------------------------------------------------------

/// Exit class for webhook forwarder subprocess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitClass {
    Terminal,
    Transient,
}

/// Classify a forwarder exit from stderr content.
pub fn classify_forwarder_exit(stderr: &str) -> ExitClass {
    let text = stderr.to_lowercase();
    for p in &[
        "http 401",
        "authentication required",
        "gh auth login",
        "http 403",
        "resource not accessible",
        "http 404",
        "hook already exists",
    ] {
        if text.contains(p) {
            return ExitClass::Terminal;
        }
    }
    if text.contains("validation failed") && text.contains("hook") {
        return ExitClass::Terminal;
    }
    ExitClass::Transient
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn run_gh(gh_path: &str, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new(gh_path).args(args).output().map_err(|e| format!("gh: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("HTTP 404") || stderr.contains("Not Found") {
            return Ok(String::new());
        }
        return Err(stderr.trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn build_hook_body(url: &str, secret: &str, events: &[&str], active: bool) -> serde_json::Value {
    serde_json::json!({
        "name": "web", "active": active, "events": events,
        "config": { "url": url, "content_type": "json", "secret": secret, "insecure_ssl": "0" }
    })
}

fn write_tmp_json(value: &serde_json::Value) -> Result<String, String> {
    let content = serde_json::to_string(value).map_err(|e| format!("serialize: {e}"))?;
    let path = std::env::temp_dir().join(format!("looper-webhook-{}.json", uuid::Uuid::new_v4()));
    std::fs::write(&path, &content).map_err(|e| format!("write tmp: {e}"))?;
    Ok(path.to_string_lossy().to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_tunnel_secret() {
        let s = derive_tunnel_secret("token", "owner/repo");
        assert_eq!(s.len(), 64);
    }

    #[test]
    fn test_classify_forwarder_terminal() {
        assert_eq!(classify_forwarder_exit("HTTP 401"), ExitClass::Terminal);
        assert_eq!(classify_forwarder_exit("HTTP 404"), ExitClass::Terminal);
        assert_eq!(classify_forwarder_exit("authentication required"), ExitClass::Terminal);
    }

    #[test]
    fn test_classify_forwarder_transient() {
        assert_eq!(classify_forwarder_exit("timeout"), ExitClass::Transient);
        assert_eq!(classify_forwarder_exit("tls error"), ExitClass::Transient);
    }

    #[test]
    fn test_build_hook_body() {
        let b = build_hook_body("https://hook.example.com", "secret", &["push", "pull_request"], true);
        assert_eq!(b["name"], "web");
        assert!(b["active"].as_bool().unwrap());
    }
}
