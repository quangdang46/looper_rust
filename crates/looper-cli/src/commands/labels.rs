//! Label management commands.

use crate::client::DaemonAPIClient;
use crate::error::CliError;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum LabelsCommand {
    /// List looper labels
    List,
}

pub async fn handle(_client: &DaemonAPIClient, cmd: &LabelsCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        LabelsCommand::List => list_labels(json).await,
    }
}

async fn list_labels(json: bool) -> Result<(), CliError> {
    let labels = vec![
        ("looper:plan", "Triggers planner for an issue"),
        ("looper:review", "Triggers reviewer for a PR"),
        ("looper:fix", "Triggers fixer for a PR"),
        ("looper:work", "Triggers worker for an issue"),
        ("looper:spec-reviewing", "Spec PR is being reviewed"),
        ("looper:spec-ready", "Spec PR is ready for implementation"),
    ];

    if json {
        let items: Vec<serde_json::Value> =
            labels.iter().map(|(name, desc)| serde_json::json!({"name": name, "description": desc})).collect();
        println!("{}", serde_json::to_string_pretty(&items).unwrap_or_default());
    } else {
        let header = format!("{:<25} {}", "Label", "Description");
        println!("{header}");
        println!("{}", "-".repeat(60));
        for (name, desc) in &labels {
            println!("{:<25} {}", name, desc);
        }
        println!();
        println!("Usage: Add these labels to GitHub issues/PRs to trigger looper actions.");
    }
    Ok(())
}
