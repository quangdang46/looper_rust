//! `looper bootstrap` — first-run setup wizard.

use crate::error::CliError;

pub async fn run(json: bool) -> Result<(), CliError> {
    if !json {
        println!("=== Looper Bootstrap ===");
        println!();
    }

    let gh_ok =
        std::process::Command::new("gh").args(["auth", "status"]).output().map(|o| o.status.success()).unwrap_or(false);
    if gh_ok {
        if !json {
            println!("✅ GitHub CLI (gh) — authenticated");
        }
    } else {
        if !json {
            println!("❌ GitHub CLI (gh) — not authenticated");
        }
        return Err(CliError::daemon_lifecycle("Run 'gh auth login' first"));
    }

    let git_ok =
        std::process::Command::new("git").args(["--version"]).output().map(|o| o.status.success()).unwrap_or(false);
    if git_ok {
        if !json {
            println!("✅ Git — installed");
        }
    } else {
        return Err(CliError::daemon_lifecycle("Git is required"));
    }

    let agent_vendors = ["claude", "codex", "opencode"];
    let mut found_agent = false;
    for vendor in &agent_vendors {
        let ok = std::process::Command::new("which").arg(vendor).output().map(|o| o.status.success()).unwrap_or(false);
        if ok {
            if !json {
                println!("✅ {} — available", vendor);
            }
            found_agent = true;
        }
    }
    if !found_agent && !json {
        println!("⚠️  No AI agent found (install claude, codex, or opencode)");
    }

    let config_path = std::env::var("HOME")
        .ok()
        .map(|h| std::path::PathBuf::from(h).join(".config").join("looper").join("looper.toml"));
    if let Some(ref cfg) = config_path {
        if cfg.exists() {
            if !json {
                println!("✅ Config found at {}", cfg.display());
            }
        } else if !json {
            println!("⚠️  No config at {}", cfg.display());
        }
    }

    let daemon_running = std::process::Command::new("pgrep")
        .arg("-f")
        .arg("looperd")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if daemon_running {
        if !json {
            println!("✅ Daemon (looperd) — running");
        }
    } else if !json {
        println!("⚠️  Daemon not running. Start with: looperd");
    }

    if !json {
        println!();
        println!("=== Next steps ===");
        println!("1. Start daemon:  looperd");
        println!("2. Add project:   looper project add <path>");
        println!("3. Create issue with label 'looper:plan'");
    }

    let result = serde_json::json!({
        "status": if daemon_running && gh_ok { "ok" } else { "issues_found" },
        "gh_authenticated": gh_ok,
        "git_installed": git_ok,
        "daemon_running": daemon_running,
        "agent_available": found_agent,
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
    }
    Ok(())
}

pub async fn status(json: bool) -> Result<(), CliError> {
    run(json).await
}
