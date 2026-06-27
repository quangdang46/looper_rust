use axum::{
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, Sse},
        Json, Response,
    },
    routing::{get, post},
    Router,
};
use futures::stream::Stream;
use serde_json::json;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::sync::broadcast;

use crate::server::db::{self, CoordinatorLeaseData, Db};
use crate::types::*;

// ── Shared server state ──────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ServerState {
    pub db: Db,
    pub admin_token: String,
    pub network_id: String,
    pub protocol_version: String,
    pub minimum_daemon_version: Option<String>,
    pub lease_ttl_seconds: u64,
    pub server_version: String,
    pub advertise_url: Option<String>,
    pub event_tx: broadcast::Sender<AuditEnvelope>,
}

// ── SSE stream ───────────────────────────────────────────────────────────────

pub struct EventStream {
    rx: broadcast::Receiver<AuditEnvelope>,
}

impl Stream for EventStream {
    type Item = Result<Event, std::convert::Infallible>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            match this.rx.try_recv() {
                Ok(envelope) => {
                    let data = serde_json::to_string(&envelope).unwrap_or_default();
                    let event = Event::default().event(envelope.event.clone()).data(data);
                    return Poll::Ready(Some(Ok(event)));
                }
                Err(broadcast::error::TryRecvError::Empty) => {
                    return Poll::Pending;
                }
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    tracing::warn!("Event stream lagged by {n} messages, skipping");
                }
                Err(broadcast::error::TryRecvError::Closed) => {
                    return Poll::Ready(None);
                }
            }
        }
    }
}

// ── Middleware ────────────────────────────────────────────────────────────────

/// Middleware that checks for a valid admin Bearer token.
pub async fn admin_auth(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    let auth = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");

    if auth != state.admin_token || auth.is_empty() {
        return Err((StatusCode::UNAUTHORIZED, Json(json!({"message": "invalid admin token"}))));
    }

    Ok(next.run(request).await)
}

/// Middleware that checks for a valid node Bearer token.
pub async fn node_auth(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    let auth = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");

    let node = state
        .db
        .get_node_by_token(auth)
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"message": "database error"}))))?;

    match node {
        Some(n) => {
            // Insert node info into request extensions for handlers that need it
            let mut req = request;
            req.extensions_mut().insert(n);
            Ok(next.run(req).await)
        }
        None => Err((StatusCode::UNAUTHORIZED, Json(json!({"message": "invalid node token"})))),
    }
}

// ── Handler helpers ──────────────────────────────────────────────────────────

fn api_ok<T: serde::Serialize>(data: T) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "data": data,
            "error": null,
        })),
    )
}

fn api_err(status: StatusCode, message: &str) -> (StatusCode, Json<serde_json::Value>) {
    (
        status,
        Json(json!({
            "ok": false,
            "data": null,
            "error": { "message": message },
        })),
    )
}

fn node_row_to_membership(row: &db::NodeRow) -> Membership {
    let capabilities: NodeCapabilities = serde_json::from_str(&row.capabilities_json).unwrap_or_default();
    let target_labels: Vec<String> = serde_json::from_str(&row.target_labels_json).unwrap_or_default();

    Membership {
        node_id: row.node_id.clone(),
        node_name: row.node_name.clone(),
        daemon_version: row.daemon_version.clone(),
        github: GitHubIdentity { numeric_id: row.github_numeric_id, login: row.github_login.clone() },
        capabilities,
        target_labels,
        joined_at: row.joined_at.clone(),
        last_heartbeat_at: row.last_heartbeat_at.clone(),
        duplicate_github_identity_warning: false,
    }
}

fn lease_data_to_coordinator_lease(data: CoordinatorLeaseData) -> CoordinatorLease {
    CoordinatorLease {
        name: data.name,
        holder_node_id: data.holder_node_id,
        fencing_token: data.fencing_token,
        expires_at: data.expires_at,
    }
}

fn detect_duplicate_github_warnings(node: &db::NodeRow, duplicates: &[i64]) -> bool {
    duplicates.contains(&node.github_numeric_id)
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// POST /v1/join - Register a new node (unauthenticated, uses join key)
pub async fn handle_join(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<JoinRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    // Validate protocol version
    if req.protocol_version != state.protocol_version {
        return Err(api_err(
            StatusCode::BAD_REQUEST,
            &format!("protocol version mismatch: expected {}, got {}", state.protocol_version, req.protocol_version),
        ));
    }

    // Validate daemon version
    if req.daemon_version.is_empty() {
        return Err(api_err(StatusCode::BAD_REQUEST, "daemon version is required"));
    }

    if let Some(ref min_version) = state.minimum_daemon_version {
        if !is_version_at_least(&req.daemon_version, min_version) {
            return Err(api_err(
                StatusCode::BAD_REQUEST,
                &format!("daemon version {} is below minimum {}", req.daemon_version, min_version),
            ));
        }
    }

    // Validate node name
    if !validate_node_name(&req.node_name) {
        return Err(api_err(
            StatusCode::BAD_REQUEST,
            "invalid node name: must be 1-32 alphanumeric, dot, underscore, or hyphen characters",
        ));
    }

    // Consume join key
    let node_id = format!("node_{}", uuid::Uuid::new_v4());
    let node_token = format!("node_{}", uuid::Uuid::new_v4().to_string().replace('-', ""));

    let consumed = state
        .db
        .consume_join_key(&req.join_key, &node_id)
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;

    if !consumed {
        return Err(api_err(StatusCode::BAD_REQUEST, "invalid or already consumed join key"));
    }

    // Check for re-join of inactive node
    let reactivated = state
        .db
        .reactivate_node(
            &req.node_name,
            &node_id,
            &node_token,
            &req.daemon_version,
            &req.github,
            &req.target_labels,
            &NodeCapabilities::default(),
        )
        .map_err(|e| api_err(StatusCode::CONFLICT, &e.to_string()))?;

    if !reactivated {
        // Check for duplicate active node name
        if let Some(_existing) = state
            .db
            .get_node_by_name(&req.node_name)
            .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?
        {
            return Err(api_err(StatusCode::CONFLICT, &format!("node name '{}' is already active", req.node_name)));
        }

        // Fresh insert
        state
            .db
            .insert_node(
                &node_id,
                &req.node_name,
                &node_token,
                &req.daemon_version,
                &req.github,
                &req.target_labels,
                &NodeCapabilities::default(),
            )
            .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    }

    let warnings: Vec<String> = Vec::new();
    let resp = JoinResponse { network_id: state.network_id.clone(), node_id, node_token, warnings };

    // Emit audit event
    let _ = state.event_tx.send(AuditEnvelope {
        event: "node.joined".to_string(),
        actor: String::new(),
        occurred_at: crate::helpers::now_iso(),
        network_id: state.network_id.clone(),
        node_id: Some(resp.node_id.clone()),
        lease_name: None,
        lease_token: None,
        payload: None,
        warnings: vec![],
    });

    Ok(api_ok(resp))
}

/// POST /v1/heartbeat - Node liveness ping (node auth)
pub async fn handle_heartbeat(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(req): Json<HeartbeatRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    // Extract node from auth
    let auth = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");

    let node = state
        .db
        .get_node_by_token(auth)
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?
        .ok_or_else(|| api_err(StatusCode::UNAUTHORIZED, "invalid node token"))?;

    // Validate protocol version
    if req.protocol_version != state.protocol_version {
        return Err(api_err(
            StatusCode::BAD_REQUEST,
            &format!("protocol version mismatch: expected {}, got {}", state.protocol_version, req.protocol_version),
        ));
    }

    state
        .db
        .update_heartbeat(&node.node_id, &req.capabilities)
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;

    // Check duplicate GitHub IDs
    let dup_ids = state.db.get_duplicate_github_ids().unwrap_or_default();
    let mut warnings: Vec<String> = Vec::new();
    if detect_duplicate_github_warnings(&node, &dup_ids) {
        warnings.push(format!(
            "duplicate GitHub identity detected for user {} (ID {})",
            node.github_login, node.github_numeric_id
        ));
    }

    // Detect identity drift
    let (drifted, reason) = detect_identity_drift(
        &GitHubIdentity { numeric_id: node.github_numeric_id, login: node.github_login.clone() },
        &req.github,
    );
    if drifted {
        warnings.push(reason);
    }

    Ok(api_ok(HeartbeatResponse { recorded_at: crate::helpers::now_iso(), warnings }))
}

/// POST /v1/leave - Deregister a node (node auth)
pub async fn handle_leave(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    let auth = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");

    let node = state
        .db
        .get_node_by_token(auth)
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?
        .ok_or_else(|| api_err(StatusCode::UNAUTHORIZED, "invalid node token"))?;

    state.db.deactivate_node(&node.node_id).map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;

    // If this node held the lease, expire it
    if let Ok(Some(lease)) = state.db.get_lease() {
        if lease.holder_node_id.as_deref() == Some(&node.node_id) {
            let _ = state.db.expire_lease(&node.node_id, lease.fencing_token);
        }
    }

    let _ = state.event_tx.send(AuditEnvelope {
        event: "node.left".to_string(),
        actor: String::new(),
        occurred_at: crate::helpers::now_iso(),
        network_id: state.network_id.clone(),
        node_id: Some(node.node_id),
        lease_name: None,
        lease_token: None,
        payload: None,
        warnings: vec![],
    });

    Ok(api_ok(json!({})))
}

/// GET /v1/status - Get node's membership + lease (node auth)
pub async fn handle_status(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    let auth = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");

    let node = state
        .db
        .get_node_by_token(auth)
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?
        .ok_or_else(|| api_err(StatusCode::UNAUTHORIZED, "invalid node token"))?;

    let all_nodes =
        state.db.list_active_nodes().map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;

    let dup_ids = state.db.get_duplicate_github_ids().unwrap_or_default();

    let memberships: Vec<Membership> = all_nodes
        .iter()
        .map(|n| {
            let mut m = node_row_to_membership(n);
            m.duplicate_github_identity_warning = detect_duplicate_github_warnings(n, &dup_ids);
            m
        })
        .collect();

    let membership = node_row_to_membership(&node);
    let lease = state.db.get_lease().unwrap_or(None).map(lease_data_to_coordinator_lease);

    let mut warnings = Vec::new();
    if detect_duplicate_github_warnings(&node, &dup_ids) {
        warnings.push(format!(
            "duplicate GitHub identity detected for user {} (ID {})",
            node.github_login, node.github_numeric_id
        ));
    }

    Ok(api_ok(NodeStatusResponse {
        network_id: state.network_id.clone(),
        membership,
        memberships,
        lease,
        webhook: None,
        warnings,
        cloud_reachable: true,
        current_github: Some(GitHubIdentity { numeric_id: node.github_numeric_id, login: node.github_login.clone() }),
        identity_drift: false,
        identity_drift_reason: String::new(),
    }))
}

// ── Coordinator lease handlers ───────────────────────────────────────────────

/// POST /v1/coordinator-lease/acquire
pub async fn handle_acquire_lease(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    let node = extract_node(&state, &headers)?;
    match state.db.acquire_lease(&node.node_id, state.lease_ttl_seconds) {
        Ok((token, expires)) => {
            emit_lease_event(&state, "lease.acquired", &node.node_id, token);
            Ok(api_ok(CoordinatorLease {
                name: "coordinator".to_string(),
                holder_node_id: Some(node.node_id),
                fencing_token: token,
                expires_at: Some(expires),
            }))
        }
        Err(_) => Err(api_err(StatusCode::CONFLICT, "lease is not vacant or expired")),
    }
}

/// POST /v1/coordinator-lease/renew
pub async fn handle_renew_lease(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(req): Json<CoordinatorLeaseRenewRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    let node = extract_node(&state, &headers)?;
    match state.db.renew_lease(&node.node_id, req.fencing_token, state.lease_ttl_seconds) {
        Ok((token, expires)) => Ok(api_ok(CoordinatorLease {
            name: "coordinator".to_string(),
            holder_node_id: Some(node.node_id),
            fencing_token: token,
            expires_at: Some(expires),
        })),
        Err(_) => Err(api_err(StatusCode::PRECONDITION_FAILED, "stale coordinator lease token")),
    }
}

/// POST /v1/coordinator-lease/handoff
pub async fn handle_handoff_lease(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(req): Json<CoordinatorLeaseHandoffRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    let node = extract_node(&state, &headers)?;
    match state.db.handoff_lease(&node.node_id, req.fencing_token, &req.target_node_id) {
        Ok(()) => {
            emit_lease_event(&state, "lease.handoff", &node.node_id, req.fencing_token);
            Ok(api_ok(json!({})))
        }
        Err(_) => Err(api_err(StatusCode::PRECONDITION_FAILED, "stale coordinator lease token")),
    }
}

/// POST /v1/coordinator-lease/expire
pub async fn handle_expire_lease(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(req): Json<FencingTokenRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    let node = extract_node(&state, &headers)?;
    match state.db.expire_lease(&node.node_id, req.fencing_token) {
        Ok(()) => {
            emit_lease_event(&state, "lease.expired", &node.node_id, req.fencing_token);
            Ok(api_ok(CoordinatorLease {
                name: "coordinator".to_string(),
                holder_node_id: None,
                fencing_token: 0,
                expires_at: None,
            }))
        }
        Err(_) => Err(api_err(StatusCode::PRECONDITION_FAILED, "stale coordinator lease token")),
    }
}

/// POST /v1/coordinator-lease/revalidate
pub async fn handle_revalidate_lease(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(req): Json<CoordinatorLeaseRevalidateRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    let node = extract_node(&state, &headers)?;

    // Only holder can revalidate
    let lease = state
        .db
        .get_lease()
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?
        .ok_or_else(|| api_err(StatusCode::NOT_FOUND, "no lease found"))?;

    if lease.holder_node_id.as_deref() != Some(&node.node_id) || lease.fencing_token != req.fencing_token {
        return Err(api_err(StatusCode::PRECONDITION_FAILED, "stale coordinator lease token"));
    }

    // Probe external URL
    let method = req.method.to_uppercase();
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;

    let resp = client
        .request(reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET), &req.url)
        .header("X-Looper-Coordinator-Fencing-Token", req.fencing_token.to_string())
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => Ok(api_ok(json!({}))),
        Ok(r) => Err(api_err(StatusCode::PRECONDITION_FAILED, &format!("revalidation probe returned {}", r.status()))),
        Err(e) => Err(api_err(StatusCode::PRECONDITION_FAILED, &format!("revalidation probe failed: {}", e))),
    }
}

/// GET /v1/events - SSE event stream (node auth)
pub async fn handle_events(State(state): State<Arc<ServerState>>) -> Sse<EventStream> {
    let rx = state.event_tx.subscribe();
    Sse::new(EventStream { rx })
}

/// GET /v1/github/webhook-secret (node auth)
pub async fn handle_webhook_secret(
    State(state): State<Arc<ServerState>>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    let secret = state.db.get_meta("webhook_secret").unwrap_or(None).unwrap_or_default();
    Ok(api_ok(WebhookSecretResponse { secret }))
}

/// POST /v1/github/webhook - Receive forwarded webhooks (HMAC auth)
pub async fn handle_webhook_forward(
    State(state): State<Arc<ServerState>>,
    Json(payload): Json<serde_json::Value>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    // Emit event to subscribers
    let event_type = payload.get("event").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
    let delivery_id = payload.get("deliveryId").and_then(|v| v.as_str()).unwrap_or("").to_string();

    let _ = state.event_tx.send(AuditEnvelope {
        event: "webhook.received".to_string(),
        actor: String::new(),
        occurred_at: crate::helpers::now_iso(),
        network_id: state.network_id.clone(),
        node_id: None,
        lease_name: None,
        lease_token: None,
        payload: Some(payload),
        warnings: vec![],
    });

    Ok(api_ok(json!({
        "deliveryId": delivery_id,
        "event": event_type,
        "result": "received"
    })))
}

/// GET /healthz - Admin health check
pub async fn handle_healthz() -> (StatusCode, Json<serde_json::Value>) {
    api_ok(json!({
        "status": "ok",
        "version": ""
    }))
}

/// GET /status - Full network status (admin)
pub async fn handle_admin_status(
    State(state): State<Arc<ServerState>>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    let all_nodes =
        state.db.list_active_nodes().map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;

    let dup_ids = state.db.get_duplicate_github_ids().unwrap_or_default();

    let memberships: Vec<Membership> = all_nodes
        .iter()
        .map(|n| {
            let mut m = node_row_to_membership(n);
            m.duplicate_github_identity_warning = detect_duplicate_github_warnings(n, &dup_ids);
            m
        })
        .collect();

    let lease = state.db.get_lease().unwrap_or(None).map(lease_data_to_coordinator_lease);

    // Count duplicate warnings for response
    let warnings: Vec<String> = dup_ids.iter().map(|id| format!("duplicate GitHub identity for ID {}", id)).collect();

    Ok(api_ok(StatusResponse { network_id: state.network_id.clone(), lease, memberships, webhook: None, warnings }))
}

/// POST /v1/join-keys - Create a new join key (admin)
pub async fn handle_create_join_key(
    State(state): State<Arc<ServerState>>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    let key = format!("join_{}", uuid::Uuid::new_v4().to_string().replace('-', ""));
    state.db.create_join_key(&key).map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    Ok(api_ok(JoinKeyResponse { join_key: key }))
}

// ── Router construction ──────────────────────────────────────────────────────

/// Build the cloud server router with all routes + middleware.
pub fn build_router(state: Arc<ServerState>) -> Router {
    let admin_routes = Router::new()
        .route("/healthz", get(handle_healthz))
        .route("/status", get(handle_admin_status))
        .route("/v1/join-keys", post(handle_create_join_key))
        .layer(middleware::from_fn_with_state(state.clone(), admin_auth));

    let node_routes = Router::new()
        .route("/v1/heartbeat", post(handle_heartbeat))
        .route("/v1/leave", post(handle_leave))
        .route("/v1/status", get(handle_status))
        .route("/v1/coordinator-lease/acquire", post(handle_acquire_lease))
        .route("/v1/coordinator-lease/renew", post(handle_renew_lease))
        .route("/v1/coordinator-lease/handoff", post(handle_handoff_lease))
        .route("/v1/coordinator-lease/expire", post(handle_expire_lease))
        .route("/v1/coordinator-lease/revalidate", post(handle_revalidate_lease))
        .route("/v1/events", get(handle_events))
        .route("/v1/github/webhook-secret", get(handle_webhook_secret))
        .layer(middleware::from_fn_with_state(state.clone(), node_auth));

    let public_routes =
        Router::new().route("/v1/join", post(handle_join)).route("/v1/github/webhook", post(handle_webhook_forward));

    Router::new().merge(admin_routes).merge(node_routes).merge(public_routes).with_state(state)
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn extract_node<'a>(
    state: &'a Arc<ServerState>,
    headers: &'a HeaderMap,
) -> Result<db::NodeRow, (StatusCode, Json<serde_json::Value>)> {
    let auth = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");

    state
        .db
        .get_node_by_token(auth)
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?
        .ok_or_else(|| api_err(StatusCode::UNAUTHORIZED, "invalid node token"))
}

fn emit_lease_event(state: &ServerState, event: &str, node_id: &str, fencing_token: i64) {
    let _ = state.event_tx.send(AuditEnvelope {
        event: event.to_string(),
        actor: String::new(),
        occurred_at: crate::helpers::now_iso(),
        network_id: state.network_id.clone(),
        node_id: Some(node_id.to_string()),
        lease_name: Some("coordinator".to_string()),
        lease_token: Some(fencing_token),
        payload: None,
        warnings: vec![],
    });
}
