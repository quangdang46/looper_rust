use crate::client::NetworkClient;
use crate::error::NetworkError;
use crate::state;
use crate::types::*;
use std::sync::Arc;

/// Manages the heartbeat loop and coordinator lease lifecycle for a daemon node.
pub struct Manager {
    pub client: NetworkClient,
    pub config: ManagerConfig,
    pub state: tokio::sync::RwLock<ManagerInner>,
}

#[derive(Debug, Clone)]
pub struct ManagerConfig {
    pub daemon_version: String,
    pub protocol_version: String,
    pub node_name: String,
    pub github: GitHubIdentity,
    pub heartbeat_interval_seconds: u64,
    pub lease_ttl_seconds: u64,
    pub coordinator_eligible: bool,
    pub routed_project_ids: Vec<String>,
    pub reviewer_projects: Vec<ReviewerProject>,
    pub local_project_count: i64,
}

#[derive(Debug, Clone, Default)]
pub struct ManagerInner {
    pub joined: bool,
    pub local_state: Option<LocalState>,
    pub current_capabilities: Option<NodeCapabilities>,
    pub current_lease: Option<CoordinatorLease>,
    pub current_memberships: Vec<Membership>,
}

impl Manager {
    pub fn new(config: ManagerConfig) -> Result<Self, NetworkError> {
        let loaded = state::load_state()?;
        let (client, inner) = match &loaded {
            Some(s) => {
                let client = NetworkClient::with_token(&s.url, &s.node_token);
                (client, ManagerInner { joined: true, local_state: loaded, ..Default::default() })
            }
            None => {
                let client = NetworkClient::new("");
                (client, ManagerInner::default())
            }
        };

        Ok(Manager { client, config, state: tokio::sync::RwLock::new(inner) })
    }

    /// Join a network with a join key.
    pub async fn join(&self, url: &str, join_key: &str) -> Result<JoinResponse, NetworkError> {
        let client = NetworkClient::new(url);
        let req = JoinRequest {
            protocol_version: self.config.protocol_version.clone(),
            daemon_version: self.config.daemon_version.clone(),
            join_key: join_key.to_string(),
            node_name: self.config.node_name.clone(),
            github: self.config.github.clone(),
            target_labels: vec![format!("looper:target:{}", self.config.node_name)],
        };

        let resp = client.join(&req).await?;

        // Save local state
        let local = LocalState {
            url: url.to_string(),
            network_id: resp.network_id.clone(),
            node_id: resp.node_id.clone(),
            node_name: self.config.node_name.clone(),
            node_token: resp.node_token.clone(),
            github: self.config.github.clone(),
        };
        state::save_state(&local)?;

        // Update inner state
        {
            let mut inner = self.state.write().await;
            inner.joined = true;
            inner.local_state = Some(local);
        }

        // Update client with token
        // Note: NetworkClient uses with_token in the constructor pattern,
        // but since self.client is &self, we need to rebuild
        // For simplicity we just use the returned client

        Ok(resp)
    }

    /// Start the heartbeat loop. Spawns a tokio task that runs until cancelled.
    pub async fn start_heartbeat(
        self: Arc<Self>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), NetworkError> {
        // Borrow state briefly to get client info
        let inner = self.state.read().await;
        if !inner.joined {
            return Err(NetworkError::NotJoined);
        }
        let has_routed = !self.config.routed_project_ids.is_empty();
        drop(inner);

        // If no routed projects, no heartbeat needed
        if !has_routed {
            return Ok(());
        }

        let mut interval =
            tokio::time::interval(std::time::Duration::from_secs(self.config.heartbeat_interval_seconds));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    break;
                }
                _ = interval.tick() => {
                    if let Err(e) = self.tick_heartbeat().await {
                        tracing::warn!("Heartbeat tick failed: {}", e);
                    }
                }
            }
        }

        Ok(())
    }

    async fn tick_heartbeat(&self) -> Result<(), NetworkError> {
        // Compute current capabilities
        let _inner = self.state.read().await;
        let dynamic_load = 0i64; // Would query local storage
        let capabilities = NodeCapabilities {
            roles: vec!["coordinator".to_string(), "worker".to_string(), "reviewer".to_string()],
            coordinator_eligible: self.config.coordinator_eligible && !self.config.routed_project_ids.is_empty(),
            routed_projects: self.config.routed_project_ids.len() as i64,
            routed_project_ids: self.config.routed_project_ids.clone(),
            reviewer_projects: self.config.reviewer_projects.clone(),
            local_projects: self.config.local_project_count,
            dynamic_load,
            identity_drift: false,
            drift_reason: String::new(),
        };

        let req = HeartbeatRequest {
            protocol_version: self.config.protocol_version.clone(),
            daemon_version: self.config.daemon_version.clone(),
            node_name: self.config.node_name.clone(),
            github: self.config.github.clone(),
            capabilities,
        };

        let _resp = self.client.heartbeat(&req).await?;

        // After successful heartbeat, fetch full status for lease reconciliation
        let status = self.client.status().await?;

        // Reconcile coordinator lease (uses status before partial move)
        self.reconcile_lease(&status).await?;

        let mut inner = self.state.write().await;
        inner.current_memberships = status.memberships;
        inner.current_lease = status.lease;
        inner.current_capabilities = Some(req.capabilities.clone());

        Ok(())
    }

    async fn reconcile_lease(&self, status: &NodeStatusResponse) -> Result<(), NetworkError> {
        let eligible = self.config.coordinator_eligible
            && !self.config.routed_project_ids.is_empty()
            && !status.identity_drift
            && self.config.github.numeric_id > 0;

        match (&status.lease, eligible) {
            // Currently hold lease and still eligible → renew
            (Some(lease), true) if lease.holder_node_id == Some(status.membership.node_id.clone()) => {
                let req = CoordinatorLeaseRenewRequest { fencing_token: lease.fencing_token };
                if let Err(e) = self.client.renew_lease(&req).await {
                    tracing::warn!("Failed to renew lease: {}", e);
                }
            }
            // Don't hold lease but eligible → acquire
            (Some(lease), true) if lease.holder_node_id.is_none() || lease.holder_node_id.as_deref() == Some("") => {
                let req = CoordinatorLeaseAcquireRequest { node_id: status.membership.node_id.clone() };
                if let Err(e) = self.client.acquire_lease(&req).await {
                    tracing::warn!("Failed to acquire lease: {}", e);
                }
            }
            // Lease vacant or expired and eligible → acquire
            (None, true) => {
                let req = CoordinatorLeaseAcquireRequest { node_id: status.membership.node_id.clone() };
                if let Err(e) = self.client.acquire_lease(&req).await {
                    tracing::warn!("Failed to acquire lease: {}", e);
                }
            }
            // Hold lease but no longer eligible → expire
            (Some(lease), false) if lease.holder_node_id == Some(status.membership.node_id.clone()) => {
                let req = FencingTokenRequest { fencing_token: lease.fencing_token };
                if let Err(e) = self.client.expire_lease(&req).await {
                    tracing::warn!("Failed to expire lease: {}", e);
                }
            }
            _ => {}
        }

        Ok(())
    }

    /// Leave the network.
    pub async fn leave(&self) -> Result<(), NetworkError> {
        self.client.leave().await?;
        state::remove_state()?;
        let mut inner = self.state.write().await;
        inner.joined = false;
        inner.local_state = None;
        Ok(())
    }
}
