//! `looper reconcile-stale` — destructive: terminates all active/running loops.
//!
//! Hidden from primary help. Requires an explicit confirmation flag because the
//! historical name implies safe stale recovery but the implementation mass-kills.

use crate::client::DaemonAPIClient;
use crate::error::CliError;

/// Flag name required before any terminate-all side effects.
pub const CONFIRM_TERMINATE_ALL_FLAG: &str = "i-know-this-terminates-all-active-loops";

pub async fn handle(client: &DaemonAPIClient, json: bool, confirm_terminate_all: bool) -> Result<(), CliError> {
    if !confirm_terminate_all {
        return Err(CliError::Other(format!(
            "refusing: 'looper reconcile-stale' terminates ALL active/running loops \
             (not just stale ones). Pass --{CONFIRM_TERMINATE_ALL_FLAG} to proceed."
        )));
    }

    let projects = client.list_projects().await?;
    let mut recovered = 0u32;
    let mut failed = 0u32;
    for proj in &projects {
        let loops = client.list_loops(&proj.name, 0, 100).await?;
        for l in &loops {
            if l.status == "active" || l.status == "running" {
                match client.terminate_loop(&proj.name, l.seq).await {
                    Ok(_) => {
                        if !json {
                            println!("Terminated loop #{} ({}) in project {}", l.seq, l.loop_type, proj.name);
                        }
                        recovered += 1;
                    }
                    Err(e) => {
                        if !json {
                            eprintln!("Failed loop #{}: {}", l.seq, e);
                        }
                        failed += 1;
                    }
                }
            }
        }
    }
    let result = serde_json::json!({"terminated": recovered, "failed": failed});
    if !json {
        println!("Terminated {recovered} loops, {failed} failures");
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reconcile_stale_errors_without_confirm_flag() {
        let client = DaemonAPIClient::new("http://127.0.0.1:7391".into(), None);
        let err = handle(&client, false, false).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(CONFIRM_TERMINATE_ALL_FLAG) || msg.contains("refusing"), "unexpected message: {msg}");
    }
}
