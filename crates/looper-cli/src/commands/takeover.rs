//! `looper takeover` — disabled stub (hidden from help).

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

pub async fn handle(_client: &DaemonAPIClient, _cmd: &TakeoverCommand, _json: bool) -> Result<(), CliError> {
    Err(CliError::unsupported("looper takeover"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn takeover_is_unsupported() {
        let client = DaemonAPIClient::new("http://127.0.0.1:7391".into(), None);
        let err = handle(&client, &TakeoverCommand::Status(TakeoverStatusArgs { run_id: "r1".into() }), false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unsupported"));
    }
}
