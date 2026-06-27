use crate::client::DaemonAPIClient;
use crate::error::CliError;

pub async fn handle(client: &DaemonAPIClient, json: bool) -> Result<(), CliError> {
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
                            println!("Recovered loop #{} ({}) in project {}", l.seq, l.loop_type, proj.name);
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
    let result = serde_json::json!({"recovered": recovered, "failed": failed});
    if !json {
        println!("Recovered {} loops, {} failures", recovered, failed);
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
    }
    Ok(())
}
