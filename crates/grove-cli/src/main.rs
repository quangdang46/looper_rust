use anyhow::{bail, Context, Result};
use camino::Utf8PathBuf;
use clap::{Parser, Subcommand};
use grove_br::{sync_bead_cache, BrClient, BrIssueDetail, CliBrClient};
use grove_bv::{BvClient, BvTriageOutput, CliBvClient};
use grove_config::{
    detect_required_tooling, load_from_workspace, GroveConfig, GrovePaths, LoadedConfig,
    RequiredTooling, ToolCapability,
};
use grove_db::Database;
use grove_kernel::{
    load_bead_inspect_view, load_workspace_status_view, BeadInspectView, WorkspaceStatusView,
};
use grove_types::{BeadId, BeadPriority};
use std::{env, fs};

#[derive(Parser)]
#[command(name = "grove")]
#[command(about = "Autonomous orchestration for beads-backed Claude work")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    Init,
    Status,
    Inspect { bead_id: String },
    Log,
    Retry,
    Run,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Init) => handle_init(),
        Some(Command::Status) => handle_status(),
        Some(Command::Inspect { bead_id }) => handle_inspect(&BeadId::new(bead_id)),
        Some(command) => {
            println!(
                "{} is not implemented yet in the Phase 1 CLI surface.",
                command_name(&command)
            );
            Ok(())
        }
        None => {
            println!("Use `grove --help` to see available commands.");
            Ok(())
        }
    }
}

fn handle_init() -> Result<()> {
    let workspace_root = current_workspace_root()?;
    let config_path = workspace_root.join("grove.toml");
    let wrote_default_config = if config_path.exists() {
        false
    } else {
        write_default_config(&config_path)?;
        true
    };

    let loaded = load_from_workspace(&workspace_root).with_context(|| {
        format!(
            "load grove configuration from {}",
            workspace_root.join("grove.toml")
        )
    })?;
    ensure_workspace_layout(&loaded.paths)?;

    let tooling = detect_required_tooling(&loaded.config);
    ensure_required_tooling(&tooling)?;

    let br = CliBrClient::new("br", loaded.paths.workspace_root().as_str());
    let bv = CliBvClient::new("bv", loaded.paths.workspace_root().as_str());
    let br_capability = br.capability().context("check br capability")?;
    let bv_capability = bv.capability().context("check bv capability")?;

    let mut db = Database::open(loaded.paths.db_path())
        .with_context(|| format!("open database at {}", loaded.paths.db_path()))?;
    db.migrate().context("apply database migrations")?;

    let synced_beads = if br_capability.beads_dir_exists {
        let sync_result = sync_bead_cache(&br, &mut db).context("sync bead cache from br")?;
        sync_result.beads_synced
    } else {
        0
    };

    println!("Initialized grove workspace.");
    println!("- database: {}", loaded.paths.db_path());
    println!("- config: {}", loaded.paths.config_path());
    println!("- transcripts: {}", loaded.paths.transcript_dir());
    println!("- checkpoints: {}", loaded.paths.checkpoints_dir());
    if wrote_default_config {
        println!("- wrote default config: {}", loaded.paths.config_path());
    }
    if br_capability.beads_dir_exists {
        println!("- bead cache synced: {synced_beads} bead(s)");
    }

    println!("\nValidated tools:");
    println!(
        "- {}",
        render_tool_line(&tooling.claude.binary, tooling.claude.version.as_deref())
    );
    println!(
        "- {}",
        render_tool_line("br", br_capability.version_line.as_deref())
    );
    println!(
        "- {}",
        render_tool_line("bv", bv_capability.version.as_deref())
    );

    if !br_capability.beads_dir_exists || !bv_capability.beads_dir_exists {
        println!("\nNotes:");
        if !br_capability.beads_dir_exists {
            println!(
                "- No .beads directory detected yet; run `br init` before `grove status` or `grove run`."
            );
        }
        if !bv_capability.beads_dir_exists {
            println!(
                "- `bv` does not see a .beads directory yet, so triage commands will not be available until beads are initialized."
            );
        }
    }

    println!("\nNext steps:");
    println!("1. Create or review beads with `br`");
    println!("2. Run `grove status`");
    println!("3. Run `grove inspect <bead-id>`");

    Ok(())
}

fn handle_status() -> Result<()> {
    let (loaded, db, br) = open_runtime()?;
    let view = load_workspace_status_view(
        &db,
        &br,
        loaded.paths.workspace_root().as_str(),
        &loaded.config,
    )
    .context("load workspace status view")?;

    let bv = CliBvClient::new("bv", loaded.paths.workspace_root().as_str());
    let (triage, triage_error) = match bv.triage() {
        Ok(output) => (Some(output), None),
        Err(error) => (None, Some(error.to_string())),
    };

    print_status_view(
        &view,
        loaded.paths.db_path().as_str(),
        triage.as_ref(),
        triage_error.as_deref(),
    );
    Ok(())
}

fn handle_inspect(bead_id: &BeadId) -> Result<()> {
    let (loaded, db, br) = open_runtime()?;
    let issue_detail = br.show(bead_id).ok();
    let view = load_bead_inspect_view(&db, &br, bead_id, &loaded.config)
        .with_context(|| format!("load inspect view for {bead_id}"))?;

    if issue_detail.is_none() && view.is_none() {
        bail!("bead {bead_id} was not found in br or the local Grove cache");
    }

    print_inspect_report(
        loaded.paths.workspace_root().as_str(),
        issue_detail.as_ref(),
        view.as_ref(),
    );
    Ok(())
}

fn open_runtime() -> Result<(LoadedConfig, Database, CliBrClient)> {
    let workspace_root = current_workspace_root()?;
    let loaded = load_from_workspace(&workspace_root).with_context(|| {
        format!(
            "load grove configuration from {}",
            workspace_root.join("grove.toml")
        )
    })?;
    ensure_workspace_layout(&loaded.paths)?;

    let br = CliBrClient::new("br", loaded.paths.workspace_root().as_str());
    let br_capability = br.capability().context("check br capability")?;
    if !br_capability.beads_dir_exists {
        bail!(
            "{} does not contain a .beads directory; run `br init` first",
            loaded.paths.workspace_root()
        );
    }

    let mut db = Database::open(loaded.paths.db_path())
        .with_context(|| format!("open database at {}", loaded.paths.db_path()))?;
    db.migrate().context("apply database migrations")?;
    sync_bead_cache(&br, &mut db).context("sync bead cache from br")?;

    Ok((loaded, db, br))
}

fn current_workspace_root() -> Result<Utf8PathBuf> {
    let cwd = env::current_dir().context("read current working directory")?;
    Utf8PathBuf::from_path_buf(cwd)
        .map_err(|path| anyhow::anyhow!("working directory is not valid UTF-8: {}", path.display()))
}

fn write_default_config(path: &Utf8PathBuf) -> Result<()> {
    let text =
        toml::to_string_pretty(&GroveConfig::default()).context("serialize default config")?;
    fs::write(path, format!("{text}\n"))
        .with_context(|| format!("write default config to {path}"))?;
    Ok(())
}

fn ensure_workspace_layout(paths: &GrovePaths) -> Result<()> {
    let mut dirs = vec![
        paths.grove_dir().to_owned(),
        paths.transcript_dir().to_owned(),
        paths.prompts_dir(),
        paths.checkpoints_dir(),
        paths.artifacts_dir(),
        paths.logs_dir(),
        paths.tmp_dir(),
    ];
    if let Some(parent) = paths.db_path().parent() {
        dirs.push(parent.to_owned());
    }

    for dir in dirs {
        fs::create_dir_all(&dir).with_context(|| format!("create managed directory {dir}"))?;
    }

    Ok(())
}

fn ensure_required_tooling(tooling: &RequiredTooling) -> Result<()> {
    for tool in [&tooling.claude, &tooling.br, &tooling.bv] {
        ensure_tool_available(tool)?;
    }
    Ok(())
}

fn ensure_tool_available(tool: &ToolCapability) -> Result<()> {
    if tool.available {
        Ok(())
    } else {
        bail!("required tool `{}` is not available on PATH", tool.binary)
    }
}

fn print_status_view(
    view: &WorkspaceStatusView,
    db_path: &str,
    triage: Option<&BvTriageOutput>,
    triage_error: Option<&str>,
) {
    println!("Workspace: {}", view.workspace_root);
    println!("Database: {db_path}");
    match &view.leader {
        Some(leader) => {
            println!("Leader: {}", leader.owner_label);
            if let Some(heartbeat_at) = leader.heartbeat_at {
                println!("Leader heartbeat: {heartbeat_at}");
            }
            if let Some(expires_at) = leader.expires_at {
                println!("Leader expires: {expires_at}");
            }
        }
        None => println!("Leader: none"),
    }

    println!("\nBeads status counts:");
    if view.bead_status_counts.is_empty() {
        println!("- none");
    } else {
        for count in &view.bead_status_counts {
            println!("- {}: {}", count.status, count.count);
        }
    }

    println!("\nGrove runtime counts:");
    if view.grove_status_counts.is_empty() {
        println!("- none");
    } else {
        for count in &view.grove_status_counts {
            println!("- {}: {}", count.status, count.count);
        }
    }

    println!("\nRunning beads:");
    if view.running_beads.is_empty() {
        println!("- none");
    } else {
        for bead in &view.running_beads {
            println!(
                "- {} [{}] {}",
                bead.bead_id,
                format_priority(bead.priority),
                bead.title
            );
            println!("  run: {}", display_option(bead.run_id.as_ref()));
            println!("  session: {}", display_option(bead.session_id.as_ref()));
            println!("  started: {}", display_option(bead.started_at.as_ref()));
            if let Some(context_pressure_pct) = bead.context_pressure_pct {
                println!("  context pressure: {:.1}%", context_pressure_pct * 100.0);
            }
            if let Some(last_progress) = bead.last_progress.as_deref() {
                println!("  progress: {last_progress}");
            }
        }
    }

    println!("\nReady queue:");
    if view.ready_queue.is_empty() {
        println!("- none");
    } else {
        for entry in &view.ready_queue {
            println!(
                "- {} [{}] score {} — {}",
                entry.bead_id,
                format_priority(entry.priority),
                format_score(entry.score),
                entry.dispatch.summary()
            );
            if !entry.why.is_empty() {
                println!("  why: {}", entry.why.join(", "));
            }
            if !entry.score_breakdown.is_empty() {
                let breakdown = entry
                    .score_breakdown
                    .iter()
                    .map(|component| format!("{}={:.1}", component.label, component.value))
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("  score breakdown: {breakdown}");
            }
            if !entry.dispatch.local_suppression_reasons.is_empty() {
                let suppressions = entry
                    .dispatch
                    .local_suppression_reasons
                    .iter()
                    .map(|reason| reason.summary.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("  suppressions: {suppressions}");
            }
            if entry.mirror_pending {
                println!("  mirror pending: yes");
            }
        }
    }

    println!("\nCheckpointed beads:");
    if view.checkpointed_beads.is_empty() {
        println!("- none");
    } else {
        for bead in &view.checkpointed_beads {
            println!("- {} {}", bead.bead_id, bead.title);
            println!("  run: {}", display_option(bead.run_id.as_ref()));
            println!(
                "  checkpoint: {}",
                display_option(bead.checkpoint_id.as_deref())
            );
            println!("  saved: {}", display_option(bead.saved_at.as_ref()));
            if let Some(progress) = bead.progress.as_deref() {
                println!("  progress: {progress}");
            }
            if let Some(next_step) = bead.next_step.as_deref() {
                println!("  next step: {next_step}");
            }
            if !bead.claimed_paths.is_empty() {
                println!("  claimed paths: {}", bead.claimed_paths.join(", "));
            }
        }
    }

    println!("\nFailed beads:");
    if view.failed_beads.is_empty() {
        println!("- none");
    } else {
        for bead in &view.failed_beads {
            println!(
                "- {} [{}] {}",
                bead.bead_id,
                format_priority(bead.priority),
                bead.title
            );
            println!("  run: {}", display_option(bead.run_id.as_ref()));
            println!(
                "  failure class: {}",
                display_option(
                    bead.failure_class
                        .as_ref()
                        .map(|class| format!("{class:?}"))
                )
            );
            println!(
                "  retry after: {}",
                display_option(bead.retry_after.as_ref())
            );
            if let Some(detail) = bead.failure_detail.as_deref() {
                println!("  detail: {detail}");
            }
            if let Some(dispatch) = bead.dispatch.as_ref() {
                println!("  dispatch: {}", dispatch.summary());
            }
            if let Some(recovery_hint) = bead.recovery_hint.as_deref() {
                println!("  recovery: {recovery_hint}");
            }
            if bead.mirror_pending {
                println!("  mirror pending: yes");
            }
        }
    }

    println!("\nReservation conflicts:");
    if view.reservation_conflicts.is_empty() {
        println!("- none");
    } else {
        for conflict in &view.reservation_conflicts {
            println!(
                "- {} requested {} but conflicts with {} holding {}",
                conflict.requested_by_bead,
                conflict.requested_pattern,
                conflict.conflicting_bead,
                conflict.held_pattern
            );
            println!(
                "  conflicting run: {}",
                display_option(conflict.conflicting_run_id.as_ref())
            );
        }
    }

    println!("\nMirror pending:");
    if view.mirror_pending.is_empty() {
        println!("- none");
    } else {
        for pending in &view.mirror_pending {
            println!("- {}", pending.bead_id);
            println!("  run: {}", display_option(pending.run_id.as_ref()));
            println!("  pending actions: {}", pending.pending_actions.join(", "));
            println!(
                "  last attempt: {}",
                display_option(pending.last_attempt_at.as_ref())
            );
            if let Some(last_error) = pending.last_error.as_deref() {
                println!("  last error: {last_error}");
            }
        }
    }

    match triage {
        Some(triage) => {
            println!("\nBV triage:");
            println!("- actionable: {}", triage.quick_ref.actionable_count);
            println!("- blocked: {}", triage.quick_ref.blocked_count);
            println!("- in progress: {}", triage.quick_ref.in_progress_count);
            if let Some(top_pick) = triage.quick_ref.top_picks.first() {
                println!("- top pick: {} — {}", top_pick.id, top_pick.title);
            } else if let Some(recommendation) = triage.recommendations.first() {
                println!(
                    "- top recommendation: {} — {}",
                    recommendation.id, recommendation.title
                );
            }
        }
        None => {
            if let Some(error) = triage_error {
                println!("\nBV triage:");
                println!("- unavailable: {error}");
            }
        }
    }
}

fn print_inspect_report(
    workspace_root: &str,
    issue_detail: Option<&BrIssueDetail>,
    view: Option<&BeadInspectView>,
) {
    println!("Workspace: {workspace_root}");

    if let Some(detail) = issue_detail {
        println!("Bead: {} — {}", detail.summary.id, detail.summary.title);
        println!("Priority: {}", format_priority(detail.summary.priority));
        println!("Type: {}", detail.summary.issue_type);
        println!("Beads status: {}", detail.summary.status);
        println!(
            "Assignee: {}",
            display_option(detail.summary.assignee.as_deref())
        );
        if !detail.summary.labels.is_empty() {
            println!("Labels: {}", detail.summary.labels.join(", "));
        }
        if let Some(closed_at) = detail.closed_at {
            println!("Closed at: {closed_at}");
        }
        if let Some(close_reason) = detail.close_reason.as_deref() {
            println!("Close reason: {close_reason}");
        }
        if let Some(description) = detail.summary.description.as_deref() {
            println!("\nDescription:\n{description}");
        }
        if !detail.comments.is_empty() {
            println!("\nComments: {}", detail.comments.len());
        }
    } else if let Some(view) = view {
        println!("Bead: {} — {}", view.bead.bead.id, view.bead.bead.title);
        println!("Priority: {}", format_priority(view.bead.bead.priority));
        println!("Type: {}", view.bead.bead.issue_type);
        println!("Beads status: {}", view.bead.bead.br_status);
        if !view.bead.bead.labels.is_empty() {
            println!("Labels: {}", view.bead.bead.labels.join(", "));
        }
    }

    match view {
        Some(view) => {
            println!("\nGrove runtime:");
            println!("- status: {:?}", view.bead.grove_status);
            println!("- synced at: {}", view.bead.synced_at);
            println!("- runtime updated: {}", view.bead.runtime_updated_at);
            println!(
                "- last run: {}",
                display_option(view.bead.last_run_id.as_ref())
            );
            println!(
                "- retry after: {}",
                display_option(view.bead.retry_after.as_ref())
            );
            if !view.bead.declared_paths.is_empty() {
                println!("- declared paths: {}", view.bead.declared_paths.join(", "));
            }

            println!("\nDependencies:");
            if view.dependencies.is_empty() {
                println!("- none");
            } else {
                for dependency in &view.dependencies {
                    println!(
                        "- {} — {} [br: {}, grove: {}]",
                        dependency.bead_id,
                        dependency.title.as_deref().unwrap_or("-"),
                        dependency.br_status.as_deref().unwrap_or("-"),
                        dependency.grove_status.as_deref().unwrap_or("-")
                    );
                }
            }

            println!("\nDependents:");
            if view.dependents.is_empty() {
                println!("- none");
            } else {
                for dependent in &view.dependents {
                    println!(
                        "- {} — {} [br: {}, grove: {}]",
                        dependent.bead_id,
                        dependent.title.as_deref().unwrap_or("-"),
                        dependent.br_status.as_deref().unwrap_or("-"),
                        dependent.grove_status.as_deref().unwrap_or("-")
                    );
                }
            }

            println!("\nLatest dispatch:");
            match view.latest_dispatch.as_ref() {
                Some(dispatch) => {
                    println!(
                        "- attempted at: {}",
                        display_option(dispatch.attempted_at.as_ref())
                    );
                    println!("- summary: {}", dispatch.dispatch.summary());
                    println!("- score: {}", format_score(dispatch.score));
                    if !dispatch.why.is_empty() {
                        println!("- why: {}", dispatch.why.join(", "));
                    }
                    if !dispatch.dispatch.local_suppression_reasons.is_empty() {
                        let suppressions = dispatch
                            .dispatch
                            .local_suppression_reasons
                            .iter()
                            .map(|reason| reason.summary.as_str())
                            .collect::<Vec<_>>()
                            .join(", ");
                        println!("- suppressions: {suppressions}");
                    }
                }
                None => println!("- none"),
            }

            println!("\nRun history:");
            if view.run_history.is_empty() {
                println!("- none");
            } else {
                for run in &view.run_history {
                    println!(
                        "- {} attempt {} [{}] started {}",
                        run.run_id, run.attempt_no, run.status, run.started_at
                    );
                    println!("  ended: {}", display_option(run.ended_at.as_ref()));
                    println!(
                        "  failure class: {}",
                        display_option(run.failure_class.as_deref())
                    );
                    println!(
                        "  failure detail: {}",
                        display_option(run.failure_detail.as_deref())
                    );
                    println!(
                        "  sessions/checkpoints: {}/{}",
                        run.session_count, run.checkpoint_count
                    );
                }
            }

            println!("\nLatest session:");
            match view.latest_session.as_ref() {
                Some(session) => {
                    println!("- session: {}", session.session_id);
                    println!("- run: {}", session.run_id);
                    println!("- status: {}", session.status);
                    println!("- started: {}", session.started_at);
                    println!("- ended: {}", display_option(session.ended_at.as_ref()));
                    println!(
                        "- stop reason: {}",
                        display_option(session.stop_reason.as_deref())
                    );
                    println!(
                        "- terminal class: {}",
                        display_option(session.terminal_class.as_deref())
                    );
                    println!("- exit code: {}", display_option(session.exit_code));
                    println!("- transcript: {}", session.transcript_path);
                    println!(
                        "- result summary: {}",
                        display_option(session.result_summary.as_deref())
                    );
                    println!(
                        "- completion indicators: {}",
                        display_option(session.completion_indicators)
                    );
                    println!("- explicit exit: {}", display_option(session.explicit_exit));
                }
                None => println!("- none"),
            }

            println!("\nLatest checkpoint:");
            match view.latest_checkpoint.as_ref() {
                Some(checkpoint) => {
                    println!("- checkpoint: {}", checkpoint.checkpoint_id);
                    println!("- run: {}", checkpoint.run_id);
                    println!("- session: {}", checkpoint.session_id);
                    println!("- progress: {}", checkpoint.progress);
                    println!("- next step: {}", checkpoint.next_step);
                    println!("- saved at: {}", checkpoint.saved_at);
                    println!("- resume generation: {}", checkpoint.resume_generation);
                }
                None => println!("- none"),
            }

            println!("\nLatest handoff:");
            match view.latest_handoff.as_ref() {
                Some(handoff) => {
                    println!("- run: {}", handoff.run_id);
                    println!("- completed at: {}", handoff.completed_at);
                    println!("- summary: {}", handoff.summary);
                    if !handoff.artifacts.is_empty() {
                        println!("- artifacts: {}", handoff.artifacts.join(", "));
                    }
                    if !handoff.lessons.is_empty() {
                        println!("- lessons: {}", handoff.lessons.join(" | "));
                    }
                    if !handoff.decisions.is_empty() {
                        println!("- decisions: {}", handoff.decisions.join(" | "));
                    }
                    if !handoff.warnings.is_empty() {
                        println!("- warnings: {}", handoff.warnings.join(" | "));
                    }
                }
                None => println!("- none"),
            }

            println!("\nMirror actions:");
            if view.mirror_actions.is_empty() {
                println!("- none");
            } else {
                for action in &view.mirror_actions {
                    println!("- {} at {}", action.action, action.created_at);
                    println!("  succeeded: {}", display_option(action.succeeded));
                    println!("  detail: {}", display_option(action.detail.as_deref()));
                }
            }

            println!("\nRetrieval summary:");
            match view.retrieval_summary.as_ref() {
                Some(summary) => {
                    println!("- conversations: {}", summary.conversation_ids.len());
                    println!("- snippets: {}", summary.snippet_count);
                    for snippet in &summary.top_snippets {
                        println!(
                            "  - conversation {} message {} score {:.2}",
                            snippet.conversation_id, snippet.message_id, snippet.score
                        );
                        println!("    file: {}", display_option(snippet.file_path.as_deref()));
                        println!("    snippet: {}", snippet.snippet);
                    }
                }
                None => println!("- none"),
            }

            println!("\nPlaybook bullets:");
            if view.playbook_bullets.is_empty() {
                println!("- none");
            } else {
                for bullet in &view.playbook_bullets {
                    println!(
                        "- {} [{} / {}] {}",
                        bullet.bullet_id, bullet.category, bullet.maturity, bullet.text
                    );
                }
            }

            println!("\nMirror pending:");
            match view.mirror_pending.as_ref() {
                Some(pending) => {
                    println!("- bead: {}", pending.bead_id);
                    println!("- run: {}", display_option(pending.run_id.as_ref()));
                    println!("- pending actions: {}", pending.pending_actions.join(", "));
                    println!(
                        "- last attempt: {}",
                        display_option(pending.last_attempt_at.as_ref())
                    );
                    println!(
                        "- last error: {}",
                        display_option(pending.last_error.as_deref())
                    );
                }
                None => println!("- none"),
            }
        }
        None => {
            println!("\nNo local Grove runtime record is available for this bead yet.");
        }
    }
}

fn render_tool_line(binary: &str, version: Option<&str>) -> String {
    match version {
        Some(version) => format!("{binary} ({version})"),
        None => binary.to_owned(),
    }
}

fn format_priority(priority: BeadPriority) -> &'static str {
    match priority {
        BeadPriority::P0 => "P0",
        BeadPriority::P1 => "P1",
        BeadPriority::P2 => "P2",
        BeadPriority::P3 => "P3",
        BeadPriority::P4 => "P4",
    }
}

fn format_score(score: Option<f64>) -> String {
    match score {
        Some(score) => format!("{score:.1}"),
        None => "-".to_owned(),
    }
}

fn display_option<T>(value: Option<T>) -> String
where
    T: std::fmt::Display,
{
    match value {
        Some(value) => value.to_string(),
        None => "-".to_owned(),
    }
}

fn command_name(command: &Command) -> &'static str {
    match command {
        Command::Init => "init",
        Command::Status => "status",
        Command::Inspect { .. } => "inspect",
        Command::Log => "log",
        Command::Retry => "retry",
        Command::Run => "run",
    }
}
