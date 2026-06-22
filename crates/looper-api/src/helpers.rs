use chrono::Utc;

/// Return the current UTC time as an ISO-8601 string suitable for SQLite.
pub fn now_iso() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}
