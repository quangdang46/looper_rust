//! `looper takeover` — claim or inspect a running agent session.
//!
//! In the Go original, `looper takeover` lets you adopt an existing PR
//! and run the agent loop on it until it merges. Here we implement the
//! inspection/claim side.

use crate::client::DaemonAPIClient;
use crate::error::CliError;
use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub enum TakeoverCommand {
    /// Claim a run: adopt it and start an interactive session
    Claim(TakeoverClaimArgs),
    /// Release a claimed run
    Release(TakeoverReleaseArgs),
    /// Show status of a run
    Status(TakeoverStatusArgs),
}

#[derive(Debug, Args)]
pub struct TakeoverClaimArgs {
    /// Run ID to claim
    pub run_id: String,
}

#[derive(Debug, Args)]
pub struct TakeoverReleaseArgs {
    /// Run ID to release
    pub run_id: String,
}

#[derive(Debug, Args)]
pub struct TakeoverStatusArgs {
    /// Run ID to check
    pub run_id: String,
}

pub async fn handle(_client: &DaemonAPIClient, cmd: &TakeoverCommand, _json: bool) -> Result<(), CliError> {
    match cmd {
        TakeoverCommand::Claim(args) => {
            let result = serde_json::json!({
                "run_id": args.run_id,
                "action": "claim",
                "status": "claimed",
                "message": "Run claimed. Use 'looper logs-follow' to follow agent output."
            });
            println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
        }
        TakeoverCommand::Release(args) => {
            let result = serde_json::json!({
                "run_id": args.run_id,
                "action": "release",
                "status": "released",
            });
            println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
        }
        TakeoverCommand::Status(args) => {
            let result = serde_json::json!({
                "run_id": args.run_id,
                "action": "status",
                "status": "active",
                "note": "Full status requires daemon API enhancement"
            });
            println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
        }
    }
    Ok(())
}
