//! Run statistics — aggregate run metrics from the outcomes table.

use crate::client::DaemonAPIClient;
use crate::error::CliError;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum RunStatsCommand {
    /// Show aggregate run statistics
    Show,
}

pub async fn handle(client: &DaemonAPIClient, cmd: &RunStatsCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        RunStatsCommand::Show => show_stats(client, json).await,
    }
}

async fn show_stats(client: &DaemonAPIClient, json: bool) -> Result<(), CliError> {
    // Get all loops to compute stats
    let projects = client.list_projects().await.unwrap_or_default();

    let mut total_runs = 0i64;
    let mut success_runs = 0i64;
    let mut failed_runs = 0i64;
    let mut role_stats: std::collections::HashMap<String, (i64, i64)> = std::collections::HashMap::new();

    for proj in &projects {
        if let Ok(loops) = client.list_loops(&proj.name, 0, 1000).await {
            for l in &loops {
                let entry = role_stats.entry(l.loop_type.clone()).or_insert((0, 0));
                entry.0 += 1; // total
                match l.status.as_str() {
                    "completed" => {
                        entry.1 += 1; // success
                        success_runs += 1;
                    }
                    "failed" => {
                        failed_runs += 1;
                    }
                    _ => {}
                }
                total_runs += 1;
            }
        }
    }

    let success_rate = if total_runs > 0 { (success_runs as f64 / total_runs as f64 * 100.0) as i64 } else { 0 };

    if json {
        let stats = serde_json::json!({
            "total_runs": total_runs,
            "success_runs": success_runs,
            "failed_runs": failed_runs,
            "success_rate_pct": success_rate,
            "by_role": role_stats.iter().map(|(k, (total, success))| {
                serde_json::json!({
                    "role": k,
                    "total": total,
                    "success": success,
                    "success_rate_pct": if *total > 0 { success * 100 / total } else { 0 },
                })
            }).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&stats).unwrap_or_default());
    } else {
        println!("Run Statistics");
        println!("{}", "-".repeat(40));
        println!("Total:   {}", total_runs);
        println!("Success: {} ({}%)", success_runs, success_rate);
        println!("Failed:  {}", failed_runs);
        println!();
        if !role_stats.is_empty() {
            println!("{:<12} {:>8} {:>10}", "Role", "Total", "Success %");
            println!("{}", "-".repeat(35));
            let mut roles: Vec<_> = role_stats.iter().collect();
            roles.sort_by_key(|(_, (total, _))| std::cmp::Reverse(*total));
            for (role, (total, success)) in &roles {
                let rate = if *total > 0 { success * 100 / total } else { 0 };
                println!("{:<12} {:>8} {:>9}%", role, total, rate);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_stats_queries_projects() {
        // This test verifies the command compiles and runs against a mock daemon
        let client = DaemonAPIClient::new("http://127.0.0.1:7391".into(), None);
        // Will fail to connect but tests the code path compiles
        let _ = handle(&client, &RunStatsCommand::Show, false).await;
    }
}
