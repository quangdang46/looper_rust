//! `looper jump <seq>` — show the worktree path for a loop.

use crate::client::DaemonAPIClient;
use crate::commands;
use crate::error::CliError;
use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub enum JumpCommand {
    /// Show worktree path for a loop
    Jump(JumpArgs),
}

#[derive(Debug, Args)]
pub struct JumpArgs {
    /// Loop sequence number
    pub seq: i64,
    /// Project name (optional, auto-detected)
    #[arg(short, long)]
    pub project: Option<String>,
}

pub async fn handle(client: &DaemonAPIClient, cmd: &JumpCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        JumpCommand::Jump(args) => {
            let project = commands::resolve_project(client, &args.project).await?;
            let detail = client.get_loop(&project, args.seq).await?;
            // Prefer top-level worktree_path from the API (worktrees table join),
            // then fall back to loop metadata for older daemons.
            let wt_path = detail
                .worktree_path
                .as_deref()
                .filter(|p| !p.is_empty())
                .or_else(|| detail.metadata.as_ref().and_then(|m| m.get("worktree_path")).and_then(|v| v.as_str()))
                .unwrap_or("(not available via API)");
            if !json {
                println!("Project: {}", project);
                println!("Loop:    #{}", args.seq);
                println!("Status:  {}", detail.status);
                println!("Worktree:");
                println!("  {}", wt_path);
                println!("\nTo open: cd {}", wt_path);
            } else {
                let result = serde_json::json!({
                    "project": project,
                    "seq": args.seq,
                    "worktree_path": wt_path,
                    "status": detail.status,
                    "runs": detail.runs.len(),
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&result).map_err(|e| CliError::daemon_lifecycle(e.to_string()))?
                );
            }
            Ok(())
        }
    }
}
