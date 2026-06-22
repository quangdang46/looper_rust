use std::fmt;

use serde::Serialize;
use serde_json::Value;

use crate::client::*;

// ---------------------------------------------------------------------------
// Output trait — each response type controls its own human-readable rendering
// ---------------------------------------------------------------------------

pub trait Output: Serialize {
    /// Human-readable table row(s). Default renders a JSON object row.
    fn table_header() -> Vec<&'static str>;
    fn table_row(&self) -> Vec<String>;
}

// ---------------------------------------------------------------------------
// Generic output function
// ---------------------------------------------------------------------------

/// Write output: JSON if `--json` flag is set, otherwise human-readable.
pub fn print_output<T: Output>(json: bool, data: &T) {
    if json {
        print_json(data);
    } else {
        print_table(data);
    }
}

pub fn print_output_vec<T: Output>(json: bool, data: &[T]) {
    if json {
        let s = serde_json::to_string_pretty(data).unwrap_or_else(|e| format!("[{e}]"));
        println!("{s}");
    } else {
        print_table_vec(data);
    }
}

pub fn print_json<T: Serialize>(data: &T) {
    let s = serde_json::to_string_pretty(data).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"));
    println!("{s}");
}

pub fn print_ok(json: bool, msg: impl fmt::Display) {
    if json {
        println!("{{\"ok\":true,\"message\":\"{msg}\"}}");
    } else {
        println!("✓ {msg}");
    }
}

pub fn print_err(err: &crate::error::CliError, json: bool) {
    if json {
        println!("{{\"ok\":false,\"error\":\"{err}\"}}");
    } else {
        eprintln!("Error: {err}");
    }
}

// ---------------------------------------------------------------------------
// Table rendering
// ---------------------------------------------------------------------------

fn print_table<T: Output>(item: &T) {
    let header = T::table_header();
    let row = item.table_row();
    let widths: Vec<usize> = header.iter().zip(row.iter())
        .map(|(h, r)| h.len().max(r.len()))
        .collect();

    // Print header
    for (i, h) in header.iter().enumerate() {
        if i > 0 { print!("  "); }
        print!("{:<width$}", h, width = widths[i]);
    }
    println!();

    // Separator
    for (i, w) in widths.iter().enumerate() {
        if i > 0 { print!("  "); }
        print!("{}", "-".repeat(*w));
    }
    println!();

    // Row
    for (i, r) in row.iter().enumerate() {
        if i > 0 { print!("  "); }
        print!("{:<width$}", r, width = widths[i]);
    }
    println!();
}

fn print_table_vec<T: Output>(items: &[T]) {
    if items.is_empty() {
        println!("(empty)");
        return;
    }
    let header = T::table_header();
    let rows: Vec<Vec<String>> = items.iter().map(|i| i.table_row()).collect();
    let widths: Vec<usize> = header.iter().enumerate()
        .map(|(col, h)| {
            let max_data = rows.iter().map(|r| r[col].len()).max().unwrap_or(0);
            h.len().max(max_data)
        })
        .collect();

    // Header
    for (i, h) in header.iter().enumerate() {
        if i > 0 { print!("  "); }
        print!("{:<width$}", h, width = widths[i]);
    }
    println!();

    // Separator
    for (i, w) in widths.iter().enumerate() {
        if i > 0 { print!("  "); }
        print!("{}", "-".repeat(*w));
    }
    println!();

    // Rows
    for row in &rows {
        for (i, r) in row.iter().enumerate() {
            if i > 0 { print!("  "); }
            print!("{:<width$}", r, width = widths[i]);
        }
        println!();
    }
}

// ---------------------------------------------------------------------------
// Output implementations for all client types
// ---------------------------------------------------------------------------

impl Output for HealthResponse {
    fn table_header() -> Vec<&'static str> { vec!["Status", "Uptime", "Version"] }
    fn table_row(&self) -> Vec<String> {
        vec![
            self.status.clone(),
            format!("{}s", self.uptime_seconds),
            self.version.clone(),
        ]
    }
}

impl Output for VersionResponse {
    fn table_header() -> Vec<&'static str> { vec!["Version"] }
    fn table_row(&self) -> Vec<String> { vec![self.version.clone()] }
}

impl Output for ProjectSummary {
    fn table_header() -> Vec<&'static str> { vec!["Name", "Path", "Schedule", "Enabled"] }
    fn table_row(&self) -> Vec<String> {
        vec![
            self.name.clone(),
            self.path.as_deref().unwrap_or("-").into(),
            self.schedule.as_deref().unwrap_or("-").into(),
            if self.enabled { "yes".into() } else { "no".into() },
        ]
    }
}

impl Output for LoopSummary {
    fn table_header() -> Vec<&'static str> { vec!["Seq", "Type", "Status", "Created"] }
    fn table_row(&self) -> Vec<String> {
        vec![
            self.seq.to_string(),
            self.loop_type.clone(),
            self.status.clone(),
            self.created_at.clone(),
        ]
    }
}

impl Output for LoopDetail {
    fn table_header() -> Vec<&'static str> { vec!["Seq", "Type", "Status", "Target", "Created"] }
    fn table_row(&self) -> Vec<String> {
        vec![
            self.seq.to_string(),
            self.loop_type.clone(),
            self.status.clone(),
            self.target.as_deref().unwrap_or("-").into(),
            self.created_at.clone(),
        ]
    }
}

impl Output for RunSummary {
    fn table_header() -> Vec<&'static str> { vec!["Run ID", "Step", "Status", "Created"] }
    fn table_row(&self) -> Vec<String> {
        vec![
            truncate(&self.run_id, 12),
            self.step_name.clone(),
            self.status.clone(),
            self.created_at.clone(),
        ]
    }
}

impl Output for RunDetail {
    fn table_header() -> Vec<&'static str> { vec!["Run ID", "Step", "Vendor", "Status", "Created"] }
    fn table_row(&self) -> Vec<String> {
        vec![
            truncate(&self.run_id, 12),
            self.step_name.clone(),
            self.agent_vendor.clone(),
            self.status.clone(),
            self.created_at.clone(),
        ]
    }
}

impl Output for QueueItemResponse {
    fn table_header() -> Vec<&'static str> { vec!["ID", "Type", "Status", "Priority", "Attempts"] }
    fn table_row(&self) -> Vec<String> {
        vec![
            truncate(&self.id, 12),
            self.queue_type.clone(),
            self.status.clone(),
            self.priority.to_string(),
            format!("{}/{}", self.attempts, "5"),
        ]
    }
}

impl Output for EventLogResponse {
    fn table_header() -> Vec<&'static str> { vec!["Timestamp", "Type", "Actor", "Details"] }
    fn table_row(&self) -> Vec<String> {
        vec![
            truncate(&self.timestamp, 19),
            self.event_type.clone(),
            self.actor.clone(),
            self.details.as_ref().map(|v| truncate(&v.to_string(), 40)).unwrap_or_default(),
        ]
    }
}

impl Output for LockResponse {
    fn table_header() -> Vec<&'static str> { vec!["Resource", "Holder", "Expires"] }
    fn table_row(&self) -> Vec<String> {
        vec![
            self.resource.clone(),
            self.holder.clone(),
            self.expires_at.as_deref().unwrap_or("-").into(),
        ]
    }
}

impl Output for ConfigResponse {
    fn table_header() -> Vec<&'static str> { vec!["Config keys"] }
    fn table_row(&self) -> Vec<String> {
        let keys: Vec<&str> = [
            self.server.as_ref().map(|_| "server"),
            self.storage.as_ref().map(|_| "storage"),
            self.agent.as_ref().map(|_| "agent"),
            self.logging.as_ref().map(|_| "logging"),
        ].into_iter().flatten().collect();
        vec![keys.join(", ")]
    }
}

impl Output for AgentConfigResponse {
    fn table_header() -> Vec<&'static str> { vec!["Project", "Vendor", "Model", "Timeout"] }
    fn table_row(&self) -> Vec<String> {
        vec![
            self.project_name.clone(),
            self.agent_vendor.clone(),
            self.model.as_deref().unwrap_or("-").into(),
            self.max_execution_seconds.map(|s| format!("{s}s")).unwrap_or_else(|| "-".into()),
        ]
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() }
    else { format!("{}…", &s[..max]) }
}

// ---------------------------------------------------------------------------
// Helper: pretty-print a Value for human display
// ---------------------------------------------------------------------------

pub fn pretty_value(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(pretty_value).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Object(map) => {
            let items: Vec<String> = map.iter()
                .map(|(k, v)| format!("{k}: {}", pretty_value(v)))
                .collect();
            format!("{{{}}}", items.join(", "))
        }
    }
}
