//! Progression wait helpers for E2E tests (D2).

use std::time::Duration;

/// Poll GET `{base}/api/projects/{project}/queue` until an item with the given
/// `queue_type` appears, or timeout.
pub fn wait_for_queue_type(
    base_url: &str,
    project: &str,
    queue_type: &str,
    timeout: Duration,
) -> Result<serde_json::Value, String> {
    let deadline = std::time::Instant::now() + timeout;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(800))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let url = format!("{base_url}/api/projects/{project}/queue?limit=100&offset=0");

    loop {
        if std::time::Instant::now() > deadline {
            return Err(format!("timeout waiting for queue type={queue_type} on project={project}"));
        }
        if let Ok(resp) = client.get(&url).send() {
            if let Ok(body) = resp.json::<serde_json::Value>() {
                if let Some(items) = body.get("data").and_then(|d| d.as_array()) {
                    if let Some(item) =
                        items.iter().find(|i| i.get("queue_type").and_then(|v| v.as_str()) == Some(queue_type))
                    {
                        eprintln!(
                            "wait_for_queue_type found type={queue_type} id={}",
                            item.get("id").and_then(|v| v.as_str()).unwrap_or("?")
                        );
                        return Ok(item.clone());
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Poll `/health` until ok:true (alias of daemon readiness contract).
pub fn wait_for_health(base_url: &str, timeout: Duration) -> Result<serde_json::Value, String> {
    let deadline = std::time::Instant::now() + timeout;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let url = format!("{base_url}/health");
    loop {
        if std::time::Instant::now() > deadline {
            return Err("timeout waiting for /health".into());
        }
        if let Ok(resp) = client.get(&url).send() {
            if resp.status().is_success() {
                if let Ok(body) = resp.json::<serde_json::Value>() {
                    if body.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                        return Ok(body);
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn wait_module_compiles() {
        // Helpers are exercised by integration tests with a live daemon.
        assert!(true);
    }
}
