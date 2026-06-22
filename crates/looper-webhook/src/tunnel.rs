use chrono::{DateTime, Utc};

use looper_storage::repos::Repositories;

/// Ping threshold: if a tunnel hasn't pinged in this many seconds, consider it unhealthy.
const TUNNEL_PING_GRACE_SECS: i64 = 300; // 5 minutes

/// Check if a tunnel has pinged recently.
pub fn is_tunnel_healthy(last_ping_at: Option<i64>, now: DateTime<Utc>) -> bool {
    match last_ping_at {
        Some(ts) => {
            let elapsed = now.timestamp() - ts;
            elapsed < TUNNEL_PING_GRACE_SECS
        }
        None => false,
    }
}

/// Health summary for a single tunnel.
#[derive(Debug, Clone)]
pub struct TunnelHealth {
    pub repo: String,
    pub healthy: bool,
    pub last_ping_at: Option<i64>,
    pub orphaned: bool,
}

/// Get health status for all tunnels.
pub fn get_tunnel_health(repos: &Repositories, now: DateTime<Utc>) -> Vec<TunnelHealth> {
    match repos.webhook_tunnel_hooks.list() {
        Ok(hooks) => hooks
            .into_iter()
            .map(|h| TunnelHealth {
                repo: h.repo.clone(),
                healthy: is_tunnel_healthy(h.last_ping_at, now) && !h.orphaned,
                last_ping_at: h.last_ping_at,
                orphaned: h.orphaned,
            })
            .collect(),
        Err(_) => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tunnel_healthy_recent_ping() {
        let now = Utc::now();
        let recent = now.timestamp() - 60; // 1 minute ago
        assert!(is_tunnel_healthy(Some(recent), now));
    }

    #[test]
    fn test_tunnel_unhealthy_old_ping() {
        let now = Utc::now();
        let old = now.timestamp() - 600; // 10 minutes ago
        assert!(!is_tunnel_healthy(Some(old), now));
    }

    #[test]
    fn test_tunnel_unhealthy_no_ping() {
        let now = Utc::now();
        assert!(!is_tunnel_healthy(None, now));
    }
}
