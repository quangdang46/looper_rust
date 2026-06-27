//! Webhook forwarder process lifecycle — gh CLI subprocess supervision.
//!
//! Ported from Go `legacy/internal/runtime/webhook_forwarder.go`.
//! Manages gh webhook forwarder subprocesses per repo with respawn on
//! transient failure and exponential backoff.

use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const MAX_RESPAWN_DELAY_SECS: u64 = 60;
const MAX_CONSECUTIVE_TRANSIENT: u32 = 10;

#[derive(Debug, Clone)]
pub struct ForwarderSupervisorConfig {
    pub gh_path: String,
    pub webhook_events: Vec<String>,
    pub max_respawn_delay_secs: u64,
    pub grace_period_secs: u64,
}

impl Default for ForwarderSupervisorConfig {
    fn default() -> Self {
        Self {
            gh_path: "gh".into(),
            webhook_events: vec!["pull_request".into(), "issue_comment".into(), "push".into(), "check_run".into()],
            max_respawn_delay_secs: MAX_RESPAWN_DELAY_SECS,
            grace_period_secs: 5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitClass {
    Terminal,
    Transient,
}

#[derive(Debug, Clone)]
pub struct ForwarderStatus {
    pub repo: String,
    pub running: bool,
    pub respawn_count: u32,
    pub last_error: Option<String>,
    pub degraded: bool,
}

struct ForwardedProcess {
    repo: String,
    child: Option<Child>,
    respawn_count: u32,
    consecutive_transient: u32,
    degraded: bool,
    last_error: Option<String>,
}

pub struct ForwarderSupervisor {
    processes: HashMap<String, ForwardedProcess>,
    config: ForwarderSupervisorConfig,
}

impl ForwarderSupervisor {
    pub fn new(config: ForwarderSupervisorConfig) -> Self {
        Self { processes: HashMap::new(), config }
    }

    fn build_args(&self, repo: &str, url: &str) -> Vec<String> {
        vec!["webhook".into(), "forwarder".into(), "--repo".into(), repo.into(), "--url".into(), url.into()]
    }

    pub fn start_forwarder(&mut self, repo: &str, endpoint_url: &str) -> Result<(), String> {
        let args = self.build_args(repo, endpoint_url);
        let child = Command::new(&self.config.gh_path)
            .args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn forwarder: {e}"))?;
        self.processes.insert(
            repo.into(),
            ForwardedProcess {
                repo: repo.into(),
                child: Some(child),
                respawn_count: 0,
                consecutive_transient: 0,
                degraded: false,
                last_error: None,
            },
        );
        Ok(())
    }

    pub fn stop_forwarder(&mut self, repo: &str) {
        if let Some(proc) = self.processes.get_mut(repo) {
            if let Some(ref mut child) = proc.child {
                let _ = child.kill();
                let _ = child.wait();
                proc.child = None;
            }
        }
    }

    pub fn stop_all(&mut self) {
        let repos: Vec<String> = self.processes.keys().cloned().collect();
        for r in repos {
            self.stop_forwarder(&r);
        }
    }

    pub fn reconcile(&mut self, configured_repos: &[String], base_url: &str) {
        for repo in configured_repos {
            let url = format!("{}/webhook/forward", base_url.trim_end_matches('/'));
            let needs_start = !self.processes.contains_key(repo);
            if needs_start {
                if let Err(e) = self.start_forwarder(repo, &url) {
                    eprintln!("start forwarder {repo}: {e}");
                }
            }
        }
        let keys: Vec<String> = self.processes.keys().cloned().collect();
        for repo in keys {
            if !configured_repos.contains(&repo) {
                self.stop_forwarder(&repo);
                self.processes.remove(&repo);
            }
        }
        let gh = self.config.gh_path.clone();
        let max_respawn = self.config.max_respawn_delay_secs;
        let u = format!("{}/webhook/forward", base_url.trim_end_matches('/'));
        let repos_to_check: Vec<String> = configured_repos.to_vec();
        let args_cache: Vec<(String, Vec<String>)> = repos_to_check
            .iter()
            .map(|r| {
                (
                    r.clone(),
                    vec!["webhook".into(), "forwarder".into(), "--repo".into(), r.clone(), "--url".into(), u.clone()],
                )
            })
            .collect();
        for (repo, args) in &args_cache {
            if let Some(proc) = self.processes.get_mut(repo) {
                if proc.degraded {
                    continue;
                }
                if let Some(ref mut child) = proc.child {
                    if let Ok(Some(_)) = child.try_wait() {
                        proc.respawn_count += 1;
                        proc.consecutive_transient += 1;
                        if proc.consecutive_transient >= MAX_CONSECUTIVE_TRANSIENT {
                            proc.degraded = true;
                            proc.last_error = Some("degraded after too many failures".into());
                        } else {
                            let delay = respawn_delay(proc.respawn_count, max_respawn);
                            std::thread::sleep(Duration::from_secs(delay));
                            match Command::new(&gh).args(args).stdout(Stdio::null()).stderr(Stdio::piped()).spawn() {
                                Ok(c) => {
                                    proc.child = Some(c);
                                    proc.last_error = None;
                                }
                                Err(e) => {
                                    proc.last_error = Some(format!("respawn: {e}"));
                                    proc.degraded = true;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    pub fn get_status(&self) -> Vec<ForwarderStatus> {
        self.processes
            .values()
            .map(|p| ForwarderStatus {
                repo: p.repo.clone(),
                running: p.child.is_some(),
                respawn_count: p.respawn_count,
                last_error: p.last_error.clone(),
                degraded: p.degraded,
            })
            .collect()
    }
}

impl Drop for ForwarderSupervisor {
    fn drop(&mut self) {
        self.stop_all();
    }
}

pub fn classify_exit(stderr: &str) -> ExitClass {
    let t = stderr.to_lowercase();
    for p in &["http 401", "auth required", "http 403", "http 404", "hook already exists"] {
        if t.contains(p) {
            return ExitClass::Terminal;
        }
    }
    ExitClass::Transient
}

fn respawn_delay(attempt: u32, max: u64) -> u64 {
    (1u64 * 2u64.pow(attempt.saturating_sub(1))).min(max)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_classify_terminal() {
        assert_eq!(classify_exit("HTTP 401"), ExitClass::Terminal);
    }
    #[test]
    fn test_classify_transient() {
        assert_eq!(classify_exit("timeout"), ExitClass::Transient);
    }
    #[test]
    fn test_respawn_delay_basic() {
        assert_eq!(respawn_delay(1, 60), 1);
        assert_eq!(respawn_delay(7, 60), 60);
    }
    #[test]
    fn test_supervisor_new() {
        assert!(ForwarderSupervisor::new(ForwarderSupervisorConfig::default()).get_status().is_empty());
    }
}
