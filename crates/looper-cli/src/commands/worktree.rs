use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

use crate::client::DaemonAPIClient;
use crate::error::CliError;
use crate::output::{self, Output};

#[derive(Debug, Subcommand)]
pub enum WorktreeCommand {
    /// Clean up stale worktrees
    Cleanup(CleanupArgs),
}

#[derive(Debug, Args)]
pub struct CleanupArgs {
    /// Include orphan worktrees (not referenced by any loop/run)
    #[arg(long)]
    pub include_orphans: Option<bool>,
    /// Retention period in days (worktrees unused for this many days are cleaned)
    #[arg(long)]
    pub retention_days: Option<u64>,
    /// Maximum number of worktrees to clean in one run
    #[arg(long)]
    pub max_per_tick: Option<usize>,
    /// Dry run: show what would be cleaned without actually doing it
    #[arg(long)]
    pub dry_run: bool,
    /// Project ID to scope cleanup to
    #[arg(long)]
    pub project_id: Option<String>,
}

pub async fn handle(client: &DaemonAPIClient, cmd: &WorktreeCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        WorktreeCommand::Cleanup(args) => {
            let input = WorktreeCleanupInput {
                include_orphans: args.include_orphans,
                retention_days: args.retention_days,
                max_per_tick: args.max_per_tick,
                dry_run: args.dry_run,
                project_id: args.project_id.clone(),
            };
            let result = client.worktree_cleanup(&input).await?;
            output::print_output(json, &result);
            Ok(())
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorktreeCleanupInput {
    pub include_orphans: Option<bool>,
    pub retention_days: Option<u64>,
    pub max_per_tick: Option<usize>,
    pub dry_run: bool,
    pub project_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorktreeCleanupResult {
    pub scanned: usize,
    pub cleaned: usize,
    pub errors: Vec<String>,
    pub summary: serde_json::Value,
}

impl Output for WorktreeCleanupResult {
    fn table_header() -> Vec<&'static str> {
        vec!["scanned", "cleaned", "errors", "summary"]
    }

    fn table_row(&self) -> Vec<String> {
        vec![
            self.scanned.to_string(),
            self.cleaned.to_string(),
            self.errors.len().to_string(),
            serde_json::to_string(&self.summary).unwrap_or_default(),
        ]
    }
}
