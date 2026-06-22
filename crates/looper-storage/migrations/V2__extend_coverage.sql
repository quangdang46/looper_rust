-- Add tables for coordinator leases, discovery cache, and node registry
-- bringing the total to 16 tables.

CREATE TABLE IF NOT EXISTS coordinator_leases (
    lease_id TEXT PRIMARY KEY,
    node_id TEXT NOT NULL,
    coordinator_type TEXT NOT NULL,
    project_id TEXT,
    repo TEXT,
    expires_at TEXT NOT NULL,
    acquired_at TEXT NOT NULL,
    renewed_at TEXT NOT NULL,
    metadata_json TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_coordinator_leases_node ON coordinator_leases (node_id, expires_at);
CREATE INDEX IF NOT EXISTS idx_coordinator_leases_project ON coordinator_leases (project_id, coordinator_type);

CREATE TABLE IF NOT EXISTS discovery_cache (
    cache_key TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    cache_data TEXT NOT NULL,
    cached_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_discovery_cache_project ON discovery_cache (project_id, expires_at);

CREATE TABLE IF NOT EXISTS node_registry (
    node_id TEXT PRIMARY KEY,
    node_name TEXT NOT NULL,
    node_type TEXT NOT NULL,
    version TEXT,
    public_key TEXT,
    last_seen_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    metadata_json TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_node_registry_last_seen ON node_registry (last_seen_at);
CREATE INDEX IF NOT EXISTS idx_node_registry_type ON node_registry (node_type);
