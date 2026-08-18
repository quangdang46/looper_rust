//! Network admin commands.

use crate::client::DaemonAPIClient;
use crate::error::CliError;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum NetadminCommand {
    /// Show network status
    Status,
}

pub async fn handle(_client: &DaemonAPIClient, cmd: &NetadminCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        NetadminCommand::Status => show_status(json).await,
    }
}

async fn show_status(json: bool) -> Result<(), CliError> {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "mode": "local",
                "message": "Network mode is local. Use loopernet for multi-node coordination.",
            })
        );
    } else {
        println!("Network Status");
        println!("{}", "-".repeat(40));
        println!("Mode: local");
        println!();
        println!("Looper is running in local mode.");
        println!("For multi-node coordination, deploy loopernet:");
        println!("  docker pull ghcr.io/nexu-io/loopernet:latest");
    }
    Ok(())
}
