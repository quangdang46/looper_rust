use rusqlite::{params, Connection};
use std::sync::{Arc, Mutex};

use crate::types::*;

/// Wrapper to make rusqlite::Connection Send + Sync for use with async server state.
#[derive(Clone)]
pub struct Db(pub Arc<Mutex<Connection>>);

impl Db {
    pub fn new(path: &str) -> Result<Self, crate::NetworkError> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let db = Db(Arc::new(Mutex::new(conn)));
        db.create_tables()?;
        Ok(db)
    }

    fn create_tables(&self) -> Result<(), crate::NetworkError> {
        let conn = self.0.lock().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS join_keys (
                join_key TEXT PRIMARY KEY,
                created_at TEXT NOT NULL,
                consumed_at TEXT,
                consumed_by_node_id TEXT
            );

            CREATE TABLE IF NOT EXISTS nodes (
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

            CREATE TABLE IF NOT EXISTS coordinator_leases (
                name TEXT PRIMARY KEY,
                holder_node_id TEXT,
                fencing_token INTEGER NOT NULL,
                expires_at TEXT
            );

            INSERT OR IGNORE INTO meta (key, value) VALUES ('network_id', '');
            INSERT OR IGNORE INTO meta (key, value) VALUES ('protocol_version', 'loopernet/v1');
            ",
        )?;
        Ok(())
    }

    // ── Meta ──────────────────────────────────────────────────────────────────

    pub fn get_meta(&self, key: &str) -> Result<Option<String>, crate::NetworkError> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare("SELECT value FROM meta WHERE key = ?1")?;
        let result = stmt.query_row(params![key], |row| row.get::<_, String>(0)).ok();
        Ok(result)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), crate::NetworkError> {
        let conn = self.0.lock().unwrap();
        conn.execute("INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)", params![key, value])?;
        Ok(())
    }

    // ── Join keys ─────────────────────────────────────────────────────────────

    pub fn create_join_key(&self, key: &str) -> Result<(), crate::NetworkError> {
        let now = crate::helpers::now_iso();
        let conn = self.0.lock().unwrap();
        conn.execute("INSERT INTO join_keys (join_key, created_at) VALUES (?1, ?2)", params![key, now])?;
        Ok(())
    }

    pub fn consume_join_key(&self, key: &str, node_id: &str) -> Result<bool, crate::NetworkError> {
        let now = crate::helpers::now_iso();
        let conn = self.0.lock().unwrap();
        let affected = conn.execute(
            "UPDATE join_keys SET consumed_at = ?1, consumed_by_node_id = ?2 WHERE join_key = ?3 AND consumed_at IS NULL",
            params![now, node_id, key],
        )?;
        Ok(affected > 0)
    }

    // ── Nodes ─────────────────────────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    pub fn insert_node(
        &self,
        node_id: &str,
        node_name: &str,
        node_token: &str,
        daemon_version: &str,
        github: &GitHubIdentity,
        target_labels: &[String],
        capabilities: &NodeCapabilities,
    ) -> Result<(), crate::NetworkError> {
        let now = crate::helpers::now_iso();
        let caps_json = serde_json::to_string(capabilities)?;
        let labels_json = serde_json::to_string(target_labels)?;
        let conn = self.0.lock().unwrap();
        conn.execute(
            "INSERT INTO nodes (node_id, node_name, node_token, daemon_version, github_numeric_id, github_login, target_labels, capabilities_json, joined_at, last_heartbeat_at, active) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, 1)",
            params![node_id, node_name, node_token, daemon_version, github.numeric_id, github.login, labels_json, caps_json, now],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn reactivate_node(
        &self,
        node_name: &str,
        node_id: &str,
        node_token: &str,
        daemon_version: &str,
        github: &GitHubIdentity,
        target_labels: &[String],
        capabilities: &NodeCapabilities,
    ) -> Result<bool, crate::NetworkError> {
        let now = crate::helpers::now_iso();
        let caps_json = serde_json::to_string(capabilities)?;
        let labels_json = serde_json::to_string(target_labels)?;
        let conn = self.0.lock().unwrap();
        let affected = conn.execute(
            "UPDATE nodes SET node_id = ?1, node_token = ?2, daemon_version = ?3, github_numeric_id = ?4, github_login = ?5, target_labels = ?6, capabilities_json = ?7, joined_at = ?8, last_heartbeat_at = ?8, active = 1 WHERE node_name = ?9 AND active = 0",
            params![node_id, node_token, daemon_version, github.numeric_id, github.login, labels_json, caps_json, now, node_name],
        )?;
        Ok(affected > 0)
    }

    pub fn get_node_by_token(&self, token: &str) -> Result<Option<NodeRow>, crate::NetworkError> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT node_id, node_name, node_token, daemon_version, github_numeric_id, github_login, target_labels, capabilities_json, joined_at, last_heartbeat_at, active FROM nodes WHERE node_token = ?1 AND active = 1",
        )?;
        let mut rows = stmt.query_map(params![token], |row| {
            Ok(NodeRow {
                node_id: row.get(0)?,
                node_name: row.get(1)?,
                node_token: row.get(2)?,
                daemon_version: row.get(3)?,
                github_numeric_id: row.get(4)?,
                github_login: row.get(5)?,
                target_labels_json: row.get(6)?,
                capabilities_json: row.get(7)?,
                joined_at: row.get(8)?,
                last_heartbeat_at: row.get(9)?,
                active: row.get::<_, i32>(10)? != 0,
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    pub fn get_node_by_name(&self, name: &str) -> Result<Option<NodeRow>, crate::NetworkError> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT node_id, node_name, node_token, daemon_version, github_numeric_id, github_login, target_labels, capabilities_json, joined_at, last_heartbeat_at, active FROM nodes WHERE node_name = ?1 AND active = 1",
        )?;
        let mut rows = stmt.query_map(params![name], |row| {
            Ok(NodeRow {
                node_id: row.get(0)?,
                node_name: row.get(1)?,
                node_token: row.get(2)?,
                daemon_version: row.get(3)?,
                github_numeric_id: row.get(4)?,
                github_login: row.get(5)?,
                target_labels_json: row.get(6)?,
                capabilities_json: row.get(7)?,
                joined_at: row.get(8)?,
                last_heartbeat_at: row.get(9)?,
                active: row.get::<_, i32>(10)? != 0,
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    pub fn get_inactive_node_by_name(&self, name: &str) -> Result<Option<NodeRow>, crate::NetworkError> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT node_id, node_name, node_token, daemon_version, github_numeric_id, github_login, target_labels, capabilities_json, joined_at, last_heartbeat_at, active FROM nodes WHERE node_name = ?1 AND active = 0",
        )?;
        let mut rows = stmt.query_map(params![name], |row| {
            Ok(NodeRow {
                node_id: row.get(0)?,
                node_name: row.get(1)?,
                node_token: row.get(2)?,
                daemon_version: row.get(3)?,
                github_numeric_id: row.get(4)?,
                github_login: row.get(5)?,
                target_labels_json: row.get(6)?,
                capabilities_json: row.get(7)?,
                joined_at: row.get(8)?,
                last_heartbeat_at: row.get(9)?,
                active: false,
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    pub fn list_active_nodes(&self) -> Result<Vec<NodeRow>, crate::NetworkError> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT node_id, node_name, node_token, daemon_version, github_numeric_id, github_login, target_labels, capabilities_json, joined_at, last_heartbeat_at, active FROM nodes WHERE active = 1",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(NodeRow {
                    node_id: row.get(0)?,
                    node_name: row.get(1)?,
                    node_token: row.get(2)?,
                    daemon_version: row.get(3)?,
                    github_numeric_id: row.get(4)?,
                    github_login: row.get(5)?,
                    target_labels_json: row.get(6)?,
                    capabilities_json: row.get(7)?,
                    joined_at: row.get(8)?,
                    last_heartbeat_at: row.get(9)?,
                    active: row.get::<_, i32>(10)? != 0,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn update_heartbeat(&self, node_id: &str, capabilities: &NodeCapabilities) -> Result<(), crate::NetworkError> {
        let now = crate::helpers::now_iso();
        let caps_json = serde_json::to_string(capabilities)?;
        let conn = self.0.lock().unwrap();
        conn.execute(
            "UPDATE nodes SET last_heartbeat_at = ?1, capabilities_json = ?2 WHERE node_id = ?3",
            params![now, caps_json, node_id],
        )?;
        Ok(())
    }

    pub fn deactivate_node(&self, node_id: &str) -> Result<(), crate::NetworkError> {
        let conn = self.0.lock().unwrap();
        conn.execute("UPDATE nodes SET active = 0 WHERE node_id = ?1", params![node_id])?;
        Ok(())
    }

    // ── Coordinator leases ────────────────────────────────────────────────────

    pub fn get_lease(&self) -> Result<Option<CoordinatorLeaseData>, crate::NetworkError> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT name, holder_node_id, fencing_token, expires_at FROM coordinator_leases WHERE name = 'coordinator'",
        )?;
        let mut rows = stmt.query_map([], |row| {
            Ok(CoordinatorLeaseData {
                name: row.get(0)?,
                holder_node_id: row.get(1)?,
                fencing_token: row.get(2)?,
                expires_at: row.get(3)?,
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    pub fn acquire_lease(&self, holder_node_id: &str, ttl_seconds: u64) -> Result<(i64, String), crate::NetworkError> {
        let now = crate::helpers::now_iso();
        let expires = crate::helpers::now_plus_seconds(ttl_seconds as i64);
        let conn = self.0.lock().unwrap();
        // Atomically update fencing_token via subquery, only if lease is vacant or expired
        let affected = conn.execute(
            "UPDATE coordinator_leases SET holder_node_id = ?1, fencing_token = (SELECT COALESCE(MAX(fencing_token), 0) + 1 FROM coordinator_leases WHERE name = 'coordinator'), expires_at = ?2 WHERE name = 'coordinator' AND (holder_node_id IS NULL OR expires_at IS NULL OR expires_at < ?3)",
            params![holder_node_id, expires, now],
        )?;
        if affected > 0 {
            // Read back
            let mut stmt =
                conn.prepare("SELECT fencing_token, expires_at FROM coordinator_leases WHERE name = 'coordinator'")?;
            let row = stmt.query_row([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))?;
            Ok(row)
        } else {
            Err(crate::NetworkError::Database("lease not vacant or expired".to_string()))
        }
    }

    pub fn renew_lease(
        &self,
        holder_node_id: &str,
        fencing_token: i64,
        ttl_seconds: u64,
    ) -> Result<(i64, String), crate::NetworkError> {
        let expires = crate::helpers::now_plus_seconds(ttl_seconds as i64);
        let conn = self.0.lock().unwrap();
        let affected = conn.execute(
            "UPDATE coordinator_leases SET expires_at = ?1 WHERE name = 'coordinator' AND holder_node_id = ?2 AND fencing_token = ?3",
            params![expires, holder_node_id, fencing_token],
        )?;
        if affected > 0 {
            Ok((fencing_token, expires))
        } else {
            Err(crate::NetworkError::StaleLeaseToken)
        }
    }

    pub fn expire_lease(&self, holder_node_id: &str, fencing_token: i64) -> Result<(), crate::NetworkError> {
        let conn = self.0.lock().unwrap();
        let affected = conn.execute(
            "UPDATE coordinator_leases SET holder_node_id = NULL, expires_at = NULL WHERE name = 'coordinator' AND holder_node_id = ?1 AND fencing_token = ?2",
            params![holder_node_id, fencing_token],
        )?;
        if affected == 0 {
            return Err(crate::NetworkError::StaleLeaseToken);
        }
        Ok(())
    }

    pub fn handoff_lease(
        &self,
        current_holder: &str,
        fencing_token: i64,
        target_node_id: &str,
    ) -> Result<(), crate::NetworkError> {
        let expires = crate::helpers::now_plus_seconds(DEFAULT_LEASE_TTL_SECONDS as i64);
        let conn = self.0.lock().unwrap();
        let affected = conn.execute(
            "UPDATE coordinator_leases SET holder_node_id = ?1, expires_at = ?3 WHERE name = 'coordinator' AND holder_node_id = ?2 AND fencing_token = ?4",
            params![target_node_id, current_holder, expires, fencing_token],
        )?;
        if affected == 0 {
            return Err(crate::NetworkError::StaleLeaseToken);
        }
        Ok(())
    }

    pub fn get_duplicate_github_ids(&self) -> Result<Vec<i64>, crate::NetworkError> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT github_numeric_id FROM nodes WHERE active = 1 AND github_numeric_id > 0 GROUP BY github_numeric_id HAVING COUNT(*) > 1",
        )?;
        let ids = stmt.query_map([], |row| row.get::<_, i64>(0))?.collect::<Result<Vec<_>, _>>()?;
        Ok(ids)
    }
}

#[derive(Debug, Clone)]
pub struct NodeRow {
    pub node_id: String,
    pub node_name: String,
    pub node_token: String,
    pub daemon_version: String,
    pub github_numeric_id: i64,
    pub github_login: String,
    pub target_labels_json: String,
    pub capabilities_json: String,
    pub joined_at: String,
    pub last_heartbeat_at: Option<String>,
    pub active: bool,
}

#[derive(Debug, Clone)]
pub struct CoordinatorLeaseData {
    pub name: String,
    pub holder_node_id: Option<String>,
    pub fencing_token: i64,
    pub expires_at: Option<String>,
}
