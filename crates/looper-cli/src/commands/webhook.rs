//! Webhook management commands.

use crate::client::DaemonAPIClient;
use crate::error::CliError;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum WebhookCommand {
    /// Show webhook forwarding status
    Status,
    /// List webhook forwarders
    List,
}

pub async fn handle(client: &DaemonAPIClient, cmd: &WebhookCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        WebhookCommand::Status => show_status(client, json).await,
        WebhookCommand::List => list_forwarders(client, json).await,
    }
}

async fn show_status(client: &DaemonAPIClient, json: bool) -> Result<(), CliError> {
    if !client.ping().await {
        return Err(CliError::daemon_not_running());
    }

    if json {
        println!(r#"{{"status":"ok","message":"daemon is running"}}"#);
    } else {
        println!("Webhook status: daemon is running");
        println!("Webhook forwarding is managed by the daemon.");
        println!("Use `looper webhook list` to see active forwarders.");
    }
    Ok(())
}

async fn list_forwarders(client: &DaemonAPIClient, json: bool) -> Result<(), CliError> {
    if !client.ping().await {
        return Err(CliError::daemon_not_running());
    }

    // Get events to find webhook-related entries
    let projects = client.list_projects().await.unwrap_or_default();
    let mut forwarders = Vec::new();

    for proj in &projects {
        if let Ok(events) = client.list_events(&proj.name, 0, 100).await {
            for e in &events {
                if e.event_type.contains("webhook") {
                    forwarders.push(serde_json::json!({
                        "project": proj.name,
                        "event_type": e.event_type,
                        "timestamp": e.timestamp,
                    }));
                }
            }
        }
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&forwarders).unwrap_or_default());
    } else if forwarders.is_empty() {
        println!("No webhook forwarders active.");
    } else {
        let header = format!("{:<20} {:<20} {}", "Project", "Event", "Timestamp");
        println!("{header}");
        println!("{}", "-".repeat(55));
        for f in &forwarders {
            println!(
                "{:<20} {:<20} {}",
                f["project"].as_str().unwrap_or("?"),
                f["event_type"].as_str().unwrap_or("?"),
                f["timestamp"].as_str().unwrap_or("?"),
            );
        }
    }
    Ok(())
}
