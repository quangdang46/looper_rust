# Module 9: looper-network (multi-node mode) — Rust Port Spec

> Derived from:
> - `internal/network/client/client.go` (132 lines) — Network client
> - `internal/network/client/manager.go` (315 lines) — Node state manager
> - `internal/network/client/state.go` (53 lines) — Local state persistence
> - `internal/network/protocol/protocol.go` (327 lines) — Protocol messages
> - `internal/network/cloud/config.go` (47 lines) — Cloud server config
> - `internal/network/cloud/http.go` (381 lines) — Cloud HTTP handlers
> - `internal/network/cloud/service.go` (685 lines) — Cloud service logic
> - `internal/networkpolicy/policy.go` (156 lines) — Claim policy

---

## 1. Network Protocol

### 1.1 Protocol Versioning

```go
const CurrentVersion = "loopernet/v1"
```

All messages carry `protocolVersion: "loopernet/v1"`. The server validates that the version matches exactly. Daemon versions are compared via semver parsing (`X.Y.Z` only, no pre-release/build metadata considered after stripping `+`/`-` suffixes).

### 1.2 Wire Format

JSON over HTTP. Content-Type: `application/json`. Auth via `Authorization: Bearer <token>` header.

### 1.3 Message Types

#### JoinRequest (sent unauthenticated)
```json
{
  "protocolVersion": "loopernet/v1",
  "daemonVersion": "1.2.3",
  "joinKey": "join_abc123...",
  "nodeName": "my-node-1",
  "github": { "numericId": 12345, "login": "user" },
  "targetLabels": ["looper:target:my-node-1"]
}
```

#### JoinResponse
```json
{
  "networkId": "net_abc123",
  "nodeId": "node_xyz789",
  "nodeToken": "node_def456...",
  "warnings": []
}
```

#### HeartbeatRequest (sent periodically by joined nodes, every 10s)
```json
{
  "protocolVersion": "loopernet/v1",
  "daemonVersion": "1.2.3",
  "nodeName": "my-node-1",
  "github": { "numericId": 12345, "login": "user" },
  "capabilities": {
    "roles": ["coordinator", "worker", "reviewer"],
    "coordinatorEligible": true,
    "routedProjects": 2,
    "routedProjectIds": ["project_1", "project_2"],
    "reviewerProjects": [
      {
        "projectId": "project_1",
        "includeDrafts": false,
        "requireReviewRequest": true,
        "enableSelfReview": false,
        "labels": [],
        "labelMode": "any"
      }
    ],
    "localProjects": 3,
    "dynamicLoad": 1,
    "identityDrift": false,
    "driftReason": ""
  }
}
```

#### HeartbeatResponse
```json
{
  "recordedAt": "2026-06-21T12:00:00.000000000Z",
  "warnings": []
}
```

#### CoordinatorLease
```json
{
  "name": "coordinator",
  "holderNodeId": "node_xyz789",
  "fencingToken": 42,
  "expiresAt": "2026-06-21T12:00:30.000000000Z"
}
```

#### NodeStatusResponse (single node view)
```json
{
  "networkId": "net_abc123",
  "membership": { "nodeId": "node_xyz789", "nodeName": "my-node-1", ... },
  "memberships": [ ... ],
  "lease": { ... },
  "webhook": {
    "deliveriesReceived": 100,
    "lastDeliveryAt": "...",
    "lastDeliveryId": "...",
    "lastEvent": "push",
    "lastRepo": "owner/repo",
    "eventSubscribers": 1
  },
  "warnings": [],
  "cloudReachable": true,
  "currentGithub": { "numericId": 12345, "login": "user" },
  "identityDrift": false,
  "identityDriftReason": ""
}
```

#### StatusResponse (admin view)
```json
{
  "networkId": "net_abc123",
  "lease": { ... },
  "memberships": [ ... ],
  "webhook": { ... },
  "warnings": []
}
```

#### AuditEnvelope (event stream)
```json
{
  "event": "lease.changed",
  "actor": "",
  "occurredAt": "2026-06-21T12:00:00Z",
  "networkId": "net_abc123",
  "nodeId": "node_xyz789",
  "leaseName": "coordinator",
  "leaseToken": 42,
  "payload": null,
  "warnings": []
}
```

### 1.4 API Endpoints

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| POST | `/v1/join` | JoinKey (in body) | Register a new node |
| POST | `/v1/heartbeat` | Node Token | Periodic node liveness |
| POST | `/v1/leave` | Node Token | Deregister a node |
| GET | `/v1/status` | Node Token | Get node's membership + lease |
| POST | `/v1/coordinator-lease/acquire` | Node Token | Acquire coordinator lease |
| POST | `/v1/coordinator-lease/renew` | Node Token | Renew held lease |
| POST | `/v1/coordinator-lease/handoff` | Node Token | Transfer lease to another node |
| POST | `/v1/coordinator-lease/expire` | Node Token | Voluntarily expire lease |
| POST | `/v1/coordinator-lease/revalidate` | Node Token | Probe external URL to validate lease |
| GET | `/v1/events` | Node Token | SSE event stream |
| GET | `/v1/github/webhook-secret` | Node Token | Get shared webhook secret |
| POST | `/v1/github/webhook` | HMAC (no token) | Receive forwarded webhooks |
| GET | `/healthz` | Admin Token | Health check |
| GET | `/status` | Admin Token | Full network status |
| POST | `/v1/join-keys` | Admin Token | Create a new join key |

### 1.5 Auth Scheme

Two auth tiers:
1. **Admin Token** — configured via `LOOPERNET_ADMIN_TOKEN` env var, used for management paths (`/healthz`, `/status`, `/v1/join-keys`)
2. **Node Token** — generated at join time (stored locally in `network.json`), used for all node operations

Bearer token extracted from `Authorization: Bearer <token>` header.

---

## 2. Handshake & Join Flow

### 2.1 Join Sequence

```
CLI (human)                        Cloud Server
  |                                     |
  |  1. admin creates join key           |
  |  POST /v1/join-keys                 |
  |  ← { "joinKey": "join_abc..." }     |
  |                                     |
  |  2. node joins with key             |
  |  POST /v1/join                     |
  |  { joinKey, nodeName, github,       |
  |    protocolVersion, daemonVersion } |
  |  ← { networkId, nodeId,            |
  |      nodeToken, warnings }          |
  |                                     |
  |  3. state saved locally:            |
  |  ~/.looper/network.json             |
  |                                     |
  |  4. every 10s: heartbeat loop       |
  |  POST /v1/heartbeat                 |
  |  ← { recordedAt, warnings }         |
```

### 2.2 Join Key Lifecycle

- Keys stored in `join_keys` SQLite table with `created_at`, `consumed_at`, `consumed_by_node_id`
- Created by admin via `POST /v1/join-keys` → returns `join_<hex>`
- One-time use: consumed atomically in a transaction during join
- Failed join (rollback) leaves key unconsumed

### 2.3 Node Token Authentication

- Server looks up `node_token` in `nodes` table where `active = 1`
- Returns `errUnauthorized` if no match
- All node-protected endpoints verify token before proceeding

### 2.4 Node Name Validation

Pattern: `^[A-Za-z0-9._-]{1,32}$`
- 1-32 characters
- Alphanumeric, dot, underscore, hyphen only
- No colons (reserved for label prefix syntax)
- Trimmed before validation

### 2.5 Protocol Compatibility

Server validates on every `JoinRequest` and `HeartbeatRequest`:
1. `protocolVersion` must equal `"loopernet/v1"`
2. `daemonVersion` must not be empty
3. If `minimumDaemonVersion` configured, the node's version must be ≥ that version (strict semver compare)

### 2.6 Node DB Schema

```sql
CREATE TABLE nodes (
  node_id TEXT PRIMARY KEY,
  node_name TEXT NOT NULL UNIQUE COLLATE NOCASE,
  node_token TEXT NOT NULL UNIQUE,
  daemon_version TEXT NOT NULL,
  github_numeric_id INTEGER NOT NULL,
  github_login TEXT NOT NULL,
  target_labels TEXT NOT NULL,
  capabilities_json TEXT NOT NULL DEFAULT '{}',
  joined_at TEXT NOT NULL,
  last_heartbeat_at TEXT,
  active INTEGER NOT NULL DEFAULT 1
);
```

### 2.7 Re-Join Handling

If `JoinRequest.NodeName` matches an existing node where `active = 0` (previously left), the existing record is reactivated with new credentials. If the name matches an active node, a `UNIQUE` constraint error fires.

---

## 3. Heartbeat & Node Management

### 3.1 Heartbeat Loop (client side)

- `Manager.Start()` loads local state from `~/.looper/network.json`
- If no routed projects, closes immediately (no heartbeat needed)
- Otherwise spawns goroutine with 10-second ticker
- Each tick:
  1. Resolve current GitHub identity via `gh api user`
  2. Compute `NodeCapabilities` from config
  3. Detect identity drift between stored and current GitHub identity
  4. Call `POST /v1/heartbeat` with capabilities
  5. On success: call `GET /v1/status` for full membership + lease
  6. Reconcile coordinator lease (acquire/renew/expire)

### 3.2 Identity Drift Detection

```go
func identityDrift(expected, current GitHubIdentity) (bool, string)
```
- If both have numeric IDs and they differ → drift
- If both have logins and they differ → drift
- Non-numeric ID (0) is treated as "unavailable"; drift not reported for missing data

### 3.3 Capabilities (computed from config)

```
NodeCapabilities:
  roles: ["coordinator", "worker", "reviewer"]
  coordinatorEligible: true (has routed projects + coordinator enabled)
  routedProjects: N
  routedProjectIds: ["project_1", ...]
  reviewerProjects: [{ projectId, includeDrafts, requireReviewRequest, enableSelfReview, labels, labelMode }]
  localProjects: N
  dynamicLoad: count of currently running runs (from storage)
  identityDrift: bool
  driftReason: string
```

---

## 4. Coordinator Lease System

### 4.1 Lease Concept

A single named lease (`"coordinator"`) per network. Only one node holds it at a time. Used to designate which node performs coordinator-level work (e.g., advanced load balancing).

### 4.2 Lease DB Schema

```sql
CREATE TABLE coordinator_leases (
  name TEXT PRIMARY KEY,
  holder_node_id TEXT,
  fencing_token INTEGER NOT NULL,
  expires_at TEXT
);
```

### 4.3 Lease Operations

| Operation | Description | Fencing Token Check |
|-----------|-------------|---------------------|
| Acquire | Claim lease when vacant or expired | Token auto-increments |
| Renew | Extend TTL of held lease | Must match current |
| Handoff | Transfer to another active node by name | Must match current |
| Expire | Voluntarily release lease | Must match current |
| Revalidate | Probe external URL to confirm lease authority | Must match current |

### 4.4 Lease TTL

- Default: 30 seconds (`protocol.DefaultLeaseTTL`)
- Configurable via `LOOPERNET_LEASE_TTL_SECONDS` (cloud server)
- Renew before expiry to maintain ownership

### 4.5 Lease Reconciliation (client side)

Executed every heartbeat tick:
1. If eligible and holds lease → **Renew**
2. If eligible and lease vacant/expired → **Acquire**
3. If holds lease but no longer eligible → **Expire**
4. If not eligible → no-op

Eligibility conditions:
- `coordinatorEligible` (from config: routed projects > 0 AND `roles.coordinator.enabled`)
- No identity drift
- Valid GitHub identity (numeric ID > 0)

### 4.6 Lease Revalidation

The lease holder can ask the server to probe an external URL:
```
POST /v1/coordinator-lease/revalidate
{ "fencingToken": 42, "url": "http://...", "method": "GET" }
```
Server sends `GET` (or specified method) with header `X-Looper-Coordinator-Fencing-Token: 42`
- 2-second timeout
- Redirects NOT followed (`http.ErrUseLastResponse`)
- If probe fails or returns non-2xx, lease treated as stale

---

## 5. Cloud Registration & Node Discovery

### 5.1 Cloud Server Config

| Field | Env Var | Default | Required | Description |
|-------|---------|---------|----------|-------------|
| ListenAddr | `LOOPERNET_LISTEN_ADDR` | `127.0.0.1:8089` | No | Listen address |
| DBPath | `LOOPERNET_DB_PATH` | — | Yes | SQLite database path |
| AdminToken | `LOOPERNET_ADMIN_TOKEN` | — | Yes | Admin bearer token |
| NetworkID | `LOOPERNET_NETWORK_ID` | auto-generated | No | Human-readable network name |
| ProtocolVersion | `LOOPERNET_PROTOCOL_VERSION` | `loopernet/v1` | No | Minimum protocol version |
| MinimumDaemonVersion | `LOOPERNET_MIN_DAEMON_VERSION` | "" | No | Minimum daemon version |
| LeaseTTLSeconds | `LOOPERNET_LEASE_TTL_SECONDS` | 30 | No | Lease time-to-live |
| ServerVersion | build-time | — | No | Build version string |
| AdvertiseURL | `LOOPERNET_ADVERTISE_URL` | "" | No | Public URL for webhook forwarding |

### 5.2 Node Discovery

Nodes discover each other through the central cloud server via:
- `GET /v1/status` (node-scoped) — returns `memberships` array of all active nodes
- `GET /status` (admin-scoped) — same full membership list

Each membership entry contains:
- `nodeId`, `nodeName` — unique identifiers
- `github` — GitHub identity (numericId, login)
- `capabilities` — roles, routed projects count, dynamic load
- `targetLabels` — target labels for work distribution
- `joinedAt`, `lastHeartbeatAt` — timestamps
- `duplicateGithubIdentityWarning` — warning flag

### 5.3 Persistent State (client side)

Stored in `~/.looper/network.json`:
```json
{
  "url": "https://cloud.example.com",
  "networkId": "net_abc123",
  "nodeId": "node_xyz789",
  "nodeName": "my-node-1",
  "nodeToken": "node_def456...",
  "github": { "numericId": 12345, "login": "user" }
}
```

### 5.4 SSE Event Stream

Nodes can subscribe to real-time events via `GET /v1/events`:
```
event: lease.changed
data: {"event":"lease.changed","leaseName":"coordinator","leaseToken":43,"nodeId":"node_new","occurredAt":"..."}

event: webhook.received
data: {"event":"webhook.received","event":"push","deliveryId":"del_...","repo":"owner/repo"}
```

Auth: node token required. If node becomes unauthorized mid-stream, connection drops.

### 5.5 Duplicate Detection

The server detects duplicate GitHub numeric IDs across active nodes and reports them as warnings. This prevents a single GitHub identity from being active on multiple nodes simultaneously (unless intended).

---

## 6. Claim Policy (Work Distribution)

### 6.1 Network Modes

```rust
enum NetworkMode {
    Off,      // Project not part of network
    Routed,   // Work is distributed via target labels
}
```

### 6.2 Worker Claim Decision

```go
func EvaluateWorker(policy ProjectPolicy, labels []string, assignees []GitHubUser) ClaimDecision
```

1. If mode is not `Routed` → **allowed** (local mode, no network policy)
2. Must have label `looper:worker-ready` on the PR
3. Must have exactly one `looper:target:<node_name>` label matching the local node
4. Local GitHub identity must be in the PR assignees list
5. Match mode: `numeric` (preferred, by GitHub user ID) or `login_fallback`

### 6.3 Reviewer Claim Decision

```go
func EvaluateReviewer(policy ProjectPolicy, labels []string, reviewRequests []GitHubUser) ClaimDecision
```

1. If mode is not `Routed` → **allowed**
2. Must have exactly one `looper:target:<node_name>` label matching local node
3. Local GitHub identity must be in the review request list
4. Match mode same as worker

### 6.4 Target Label Resolution

```go
const TargetLabelPrefix = "looper:target:"
```

- `ParseTargetLabel(label)` — extracts node name from `looper:target:node_name`
- `TargetLabelForNode(nodeName)` — builds `looper:target:<nodeName>`
- `CollectTargetLabels(labels)` — filters array for target labels
- `PlanExactTarget(labels, nodeName)` — produces add/remove diff to make labels exactly match one target
- `HasExactTarget(labels, nodeName)` — checks if correct target label exists

### 6.5 Identity Matching

```go
func matchLocalIdentity(policy ProjectPolicy, users []GitHubUser) (bool, MatchMode)
```

Priority order:
1. **Numeric match**: compare stored `GitHubUserID` with each user's `ID` (when both > 0)
2. **Login fallback**: compare normalized (lowercased) `GitHubLogin` with each user's login
3. **Match modes**: `none`, `numeric`, `login_fallback`

### 6.6 Claim Decision Result

```rust
struct ClaimDecision {
    allowed: bool,
    reason: String,         // why disallowed (empty when allowed)
    match_mode: MatchMode,  // how identity was matched
    target_label: String,   // the matched target label
}
```

### 6.7 Project Policy Resolution

```go
func ProjectPolicyForProject(cfg config.Config, projectID string) ProjectPolicy
```

Looks up project by ID and returns its network mode. Falls back to `NetworkModeOff` if not found.

---

## 7. Rust Implementation Notes

### 7.1 Types to Define

```rust
// Protocol types — all Serialize + Deserialize
struct GitHubIdentity { numeric_id: i64, login: String }
struct NodeCapabilities { roles: Vec<String>, coordinator_eligible: bool, ... }
struct JoinRequest { protocol_version: String, daemon_version: String, ... }
struct JoinResponse { network_id: String, node_id: String, node_token: String, warnings: Vec<String> }
struct HeartbeatRequest { ... }
struct HeartbeatResponse { recorded_at: DateTime<Utc>, warnings: Vec<String> }
struct Membership { node_id: String, node_name: String, github: GitHubIdentity, ... }
struct CoordinatorLease { name: String, holder_node_id: String, fencing_token: i64, expires_at: Option<DateTime<Utc>> }
struct NodeStatusResponse { ... }
struct StatusResponse { ... }
struct AuditEnvelope { event: String, actor: String, occurred_at: DateTime<Utc>, ... }

// Policy types
enum NetworkMode { Off, Routed }
enum MatchMode { None, Numeric, LoginFallback }
struct ClaimDecision { allowed: bool, reason: String, match_mode: MatchMode, target_label: String }
struct ProjectPolicy { mode: NetworkMode, node_name: String, github_login: String, github_user_id: i64 }

// Client types
struct NetworkClient { base_url: String, node_token: String, http_client: reqwest::Client }
struct LocalState { url: String, network_id: String, node_id: String, node_name: String, node_token: String, github: GitHubIdentity }
struct Manager { state: Option<LocalState>, config: Config, ... }
```

### 7.2 Network Client Methods

```rust
impl NetworkClient {
    fn join(&self, req: JoinRequest) -> Result<JoinResponse>;
    fn heartbeat(&self, req: HeartbeatRequest) -> Result<HeartbeatResponse>;
    fn leave(&self) -> Result<()>;
    fn status(&self) -> Result<NodeStatusResponse>;
    fn acquire_lease(&self, req: CoordinatorLeaseAcquireRequest) -> Result<CoordinatorLease>;
    fn renew_lease(&self, req: CoordinatorLeaseRenewRequest) -> Result<CoordinatorLease>;
    fn expire_lease(&self, fencing_token: i64) -> Result<CoordinatorLease>;
    fn revalidate_lease(&self, req: CoordinatorLeaseRevalidateRequest) -> Result<()>;
    fn webhook_secret(&self) -> Result<String>;
}
```

### 7.3 Error Handling

- All API errors return status code + optional JSON `{"message": "..."}`
- Auth errors → 401 Unauthorized
- Stale lease token → 412 Precondition Failed with "stale coordinator lease token" message
- Protocol/version errors → 400 Bad Request
- Generic errors → 400 Bad Request

### 7.4 Persistence

- `LocalState` stored as JSON at `~/.looper/network.json` (permissions 0600)
- Must handle: file not found (not joined), read/write errors, concurrent access (single CLI process, but design for safety)

### 7.5 Concurrency

- `Manager` uses `RwLock` for status reads/writes
- Lease operations use `Mutex` on server side
- Event subscription uses map of channels with mutex
- Heartbeat loop runs in its own task

### 7.6 Target Label Validation

- Node names: `^[A-Za-z0-9._-]{1,32}$`
- Labels prefixed with `looper:target:` are target labels
- Only one target label per PR/work item allowed (policy rejects multiple)

### 7.7 Version Comparison

Semver comparison (strict): `^v?(\d+)\.(\d+)\.(\d+)([-+].*)?$`
- Only major.minor.patch compared
- Pre-release/build metadata stripped before comparison
- Used for minimum daemon version enforcement

### 7.8 Cloud Server (for Rust port reference)

- HTTP server with SQLite backend
- Tables: `meta`, `join_keys`, `nodes`, `coordinator_leases`
- WAL journal mode enabled
- Auth: admin token (env-configured) and node token (runtime-generated)
- Middleware: `adminOnly` (checks `Authorization: Bearer <adminToken>`), `nodeOnly` (checks `Authorization: Bearer <nodeToken>`)
