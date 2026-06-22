/// Current UTC timestamp as ISO 8601 string with nanoseconds and Z suffix.
pub fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.9fZ").to_string()
}

/// Returns an ISO 8601 timestamp `seconds` from now.
pub fn now_plus_seconds(seconds: i64) -> String {
    let dt = chrono::Utc::now() + chrono::Duration::seconds(seconds);
    dt.format("%Y-%m-%dT%H:%M:%S%.9fZ").to_string()
}
