use crate::{
    AcquireReservationInput, DispatchEligibilityContext, LeaderLeaseConfig, LeaderLeaseManager,
    PersistedTaskRunOutcome, ReservationManager, execute_persisted_single_task_session,
};
use anyhow::{Context, Result};
use grove_br::BrClient;
use grove_config::GroveConfig;
use grove_db::Database;
use camino::{Utf8Path, Utf8PathBuf};
use grove_session::{
    ClaudeBackend, ContextMonitor, ExitPolicy, SessionShutdownConfig, SingleTaskSessionRequest,
};
use grove_types::{
    BeadId, CircuitState, CoordinatorStopReason, ExecutionContract, GroveBeadRecord,
    GroveBeadStatus, PromptId, ReservationMode, RunId, SessionId, Timestamp,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

/// Thread-safe shutdown signal for the coordinator.
///
/// Set from a signal handler (SIGINT/SIGTERM/Ctrl-C) and polled each
/// dispatch cycle. This enables graceful shutdown: the loop stops
/// dispatching new work, persists pending state, and releases the
/// leader lease.
#[derive(Debug, Clone)]
pub struct ShutdownSignal {
    flag: Arc<AtomicBool>,
}

impl ShutdownSignal {
    /// Create a new shutdown signal (not triggered).
    #[must_use]
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Trigger the shutdown signal.
    pub fn trigger(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// Check whether shutdown has been requested.
    #[must_use]
    pub fn is_triggered(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn shared_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.flag)
    }

    /// Register this signal with the ctrlc handler.
    /// Returns an error if the handler cannot be set.
    pub fn register_ctrlc(&self) -> Result<()> {
        let flag = self.flag.clone();
        ctrlc::set_handler(move || {
            flag.store(true, Ordering::SeqCst);
            eprintln!("\ngrove: shutdown signal received, stopping after current dispatch...");
        })
        .context("register Ctrl-C handler")?;
        Ok(())
    }
}

impl Default for ShutdownSignal {
    fn default() -> Self {
        Self::new()
    }
}

/// Outcome of a single dispatch loop run.
#[derive(Debug, Clone)]
pub struct DispatchLoopOutcome {
    /// Total number of dispatches completed in this loop run.
    pub dispatched_count: u32,
    /// Total number of poll cycles executed.
    pub poll_cycles: u32,
    /// Reason the dispatch loop terminated.
    pub exit_reason: DispatchExitReason,
    /// Durable stop reason for post-mortem analysis.
    pub stop_reason: CoordinatorStopReason,
}

/// Why the dispatch loop stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchExitReason {
    /// No more dispatchable beads remain.
    QueueEmpty,
    /// Reached the maximum number of total dispatches.
    MaxRunsReached,
    /// Leader lease was contested/lost.
    LeaderContested,
    /// The configured max poll cycles were exceeded.
    MaxPollCycles { limit: u32 },
    /// Shutdown signal received (SIGINT/SIGTERM/Ctrl-C).
    ShutdownRequested,
}

impl std::fmt::Display for DispatchExitReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QueueEmpty => write!(f, "no dispatchable beads remain"),
            Self::MaxRunsReached => write!(f, "reached max total runs"),
            Self::LeaderContested => write!(f, "leader lease contested"),
            Self::MaxPollCycles { limit } => write!(f, "exceeded max poll cycles ({limit})"),
            Self::ShutdownRequested => write!(f, "shutdown signal received"),
        }
    }
}

impl DispatchExitReason {
    /// Convert to a durable `CoordinatorStopReason`.
    #[must_use]
    pub fn to_stop_reason(&self) -> CoordinatorStopReason {
        match self {
            Self::QueueEmpty => CoordinatorStopReason::QueueEmpty,
            Self::MaxRunsReached => CoordinatorStopReason::MaxRunsReached,
            Self::LeaderContested => CoordinatorStopReason::LeaderContested,
            Self::MaxPollCycles { .. } => CoordinatorStopReason::MaxPollCycles,
            Self::ShutdownRequested => CoordinatorStopReason::UserStopped,
        }
    }
}

/// Configuration for the dispatch loop beyond what `GroveConfig` provides.
#[derive(Debug, Clone)]
pub struct DispatchLoopConfig {
    /// Maximum total dispatches before the loop exits. `None` means unlimited.
    pub max_total_runs: Option<u32>,
    /// Maximum poll cycles before the loop exits. `None` means unlimited.
    pub max_poll_cycles: Option<u32>,
    /// Working directory for session execution.
    pub working_dir: Utf8PathBuf,
    /// Shutdown signal for graceful termination.
    pub shutdown_signal: ShutdownSignal,
    /// Database path so worker threads can open independent SQLite connections.
    pub db_path: Utf8PathBuf,
}

#[derive(Debug, Clone)]
struct DispatchedWorkerContext {
    bead_id: BeadId,
    run_id: RunId,
    session_id: SessionId,
}

#[derive(Debug)]
struct CompletedWorker {
    ctx: DispatchedWorkerContext,
    result: Result<PersistedTaskRunOutcome, String>,
}

struct InFlightWorker {
    handle: JoinHandle<()>,
}

fn apply_reaction_side_effects(
    db: &mut Database,
    config: &GroveConfig,
    ctx: &DispatchedWorkerContext,
    outcome: Option<&PersistedTaskRunOutcome>,
    error_detail: Option<&str>,
) {
    let Some(run) = outcome.map(|outcome| &outcome.run) else {
        return;
    };

    let inferred_activity = outcome
        .map(|outcome| crate::reactions::infer_agent_activity(&outcome.session, run.status))
        .unwrap_or_else(|| match run.failure_class {
            Some(grove_types::FailureClass::PermissionDenied) => grove_types::AgentActivity::Blocked,
            Some(grove_types::FailureClass::Interrupted | grove_types::FailureClass::ClaudeCrashed) => grove_types::AgentActivity::Exited,
            Some(grove_types::FailureClass::NoProgress) => grove_types::AgentActivity::Idle,
            _ if matches!(run.status, grove_types::RunStatus::Succeeded | grove_types::RunStatus::Checkpointed) => grove_types::AgentActivity::Ready,
            _ => grove_types::AgentActivity::Exited,
        });

    let trigger_ctx = crate::reactions::TriggerContext {
        bead_id: ctx.bead_id.clone(),
        run_id: ctx.run_id.clone(),
        run_status: run.status,
        activity: inferred_activity,
        failure_class: run.failure_class,
        failure_detail: run.failure_detail.clone().or_else(|| error_detail.map(str::to_owned)),
        escalation_tier: run.escalation_tier,
        consecutive_failures: if matches!(run.status, grove_types::RunStatus::Failed | grove_types::RunStatus::WaitingToRetry) {
            3
        } else {
            0
        },
        circuit_state: if run.failure_class == Some(grove_types::FailureClass::NoProgress) {
            grove_types::CircuitState::Open
        } else {
            grove_types::CircuitState::Closed
        },
        context_pressure_pct: outcome.and_then(|outcome| outcome.session.context_pressure_pct),
    };

    let rules = crate::reactions::load_reaction_rules(config);
    let reaction_eval = crate::reactions::evaluate_reactions(db, &trigger_ctx, &rules);

    let _ = db.update_run_activity(&ctx.bead_id, &ctx.run_id, inferred_activity, &chrono::Utc::now());
    if reaction_eval.new_tier != run.escalation_tier {
        let _ = db.update_run_escalation_tier(&ctx.bead_id, &ctx.run_id, reaction_eval.new_tier, &chrono::Utc::now());
    }

    for record in reaction_eval.records {
        let _ = db.write_event_log(
            grove_types::EventKind::ReactionInvoked,
            Some(&ctx.bead_id),
            Some(&ctx.run_id),
            Some(&ctx.session_id),
            &serde_json::to_value(&record).unwrap_or_else(|_| serde_json::json!({})),
            &chrono::Utc::now(),
        );

        if let grove_types::ReactionAction::RetryWithMutation { .. } = record.action {
            if let Some(session_outcome) = outcome.map(|outcome| &outcome.session) {
                let plan = grove_session::plan_retry_mutation(
                    run.failure_class.unwrap_or(grove_types::FailureClass::Unknown),
                    Some(session_outcome),
                );
                let _ = db.write_recovery_capsule(grove_db::RecoveryCapsuleWriteInput {
                    bead_id: ctx.bead_id.clone(),
                    run_id: ctx.run_id.clone(),
                    capsule: grove_types::RecoveryCapsule::from_parts(
                        grove_types::RecoveryCapsuleOutcome::Failed,
                        run.failure_class,
                        run.failure_detail.as_deref(),
                        None,
                        None,
                        Some(plan.next_contract.as_str()),
                        Some(plan.retry_delta_summary.as_str()),
                        &[],
                    )
                    .unwrap_or_else(|| grove_types::RecoveryCapsule {
                        outcome: grove_types::RecoveryCapsuleOutcome::Failed,
                        summary: plan.rescue_card,
                        strongest_evidence: Vec::new(),
                        likely_root_causes: Vec::new(),
                        risky_paths: Vec::new(),
                        do_not_repeat: Vec::new(),
                        next_attempt_contract: Some(plan.next_contract.as_str().to_owned()),
                        retry_delta_summary: Some(plan.retry_delta_summary),
                        checkpoint_progress: None,
                        checkpoint_next_step: None,
                        artifacts: Vec::new(),
                    }),
                    created_at: chrono::Utc::now(),
                });
            }
        }
    }
}

/// Score a single bead for dispatch priority using the same logic as `status_view`.
#[must_use]
fn score_bead(bead: &GroveBeadRecord, config: &GroveConfig) -> f64 {
    let mut score = match bead.bead.priority {
        grove_types::BeadPriority::P0 => 100.0,
        grove_types::BeadPriority::P1 => 75.0,
        grove_types::BeadPriority::P2 => 50.0,
        grove_types::BeadPriority::P3 => 25.0,
        grove_types::BeadPriority::P4 => 10.0,
    };

    if bead.grove_status == GroveBeadStatus::WaitingToRetry {
        score -= f64::from(config.scheduler.retry_penalty);
    }

    score
}

/// Select the highest-scored dispatchable bead from the ready list.
#[cfg(test)]
fn select_best_candidate<'a>(
    beads: &'a [GroveBeadRecord],
    ready_ids: &HashSet<BeadId>,
    config: &GroveConfig,
    now: Timestamp,
) -> Option<&'a GroveBeadRecord> {
    let excluded_ids = HashSet::new();
    select_best_candidate_excluding(beads, ready_ids, &excluded_ids, config, now)
}

fn select_best_candidate_excluding<'a>(
    beads: &'a [GroveBeadRecord],
    ready_ids: &HashSet<BeadId>,
    excluded_ids: &HashSet<BeadId>,
    config: &GroveConfig,
    now: Timestamp,
) -> Option<&'a GroveBeadRecord> {
    let mut candidates: Vec<_> = beads
        .iter()
        .filter(|bead| {
            !excluded_ids.contains(&bead.bead.id)
                && {
                    let eligibility = crate::evaluate_dispatch_eligibility(
                        bead,
                        &DispatchEligibilityContext {
                            ready_in_br: ready_ids.contains(&bead.bead.id),
                            circuit_state: CircuitState::Closed,
                            reservation_conflicts: Vec::new(),
                            now,
                        },
                    );
                    eligibility.dispatchable_in_grove
                }
        })
        .collect();

    candidates.sort_by(|a, b| {
        let score_a = score_bead(a, config);
        let score_b = score_bead(b, config);
        score_b
            .partial_cmp(&score_a)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.bead.id.cmp(&b.bead.id))
    });

    candidates.into_iter().next()
}

/// Build a `SingleTaskSessionRequest` from a dispatched bead.
fn build_session_request(
    bead: &GroveBeadRecord,
    config: &GroveConfig,
    working_dir: &Utf8Path,
    run_id: &RunId,
    session_id: &SessionId,
    parent_handoffs: Vec<String>,
) -> SingleTaskSessionRequest {
    let prompt_id = PromptId::new(format!("prompt-{}", run_id.as_str()));
    let transcript_path = Utf8PathBuf::from(format!(
        ".grove/transcripts/{}/{}.jsonl",
        bead.bead.id.as_str(),
        session_id.as_str()
    ));
    let prompt_manifest_path = Utf8PathBuf::from(format!(
        ".grove/prompts/{}.json",
        prompt_id.as_str()
    ));

    SingleTaskSessionRequest {
        bead_id: bead.bead.id.clone(),
        run_id: run_id.clone(),
        session_id: session_id.clone(),
        prompt_id,
        task_title: bead.bead.title.clone(),
        task_description: bead
            .bead
            .description
            .clone()
            .unwrap_or_default(),
        contract: ExecutionContract::SingleTask,
        model: config.runtime.default_model.clone(),
        working_dir: working_dir.to_owned(),
        transcript_path,
        prompt_manifest_path,
        timeout: Duration::from_secs(config.runtime.timeout_minutes * 60),
        exit_policy: ExitPolicy {
            completion_indicator_threshold: config.exit_policy.completion_indicator_threshold,
            require_explicit_exit: config.exit_policy.require_explicit_exit,
        },
        context_monitor: ContextMonitor::new(
            config.checkpoint.warn_pct,
            config.checkpoint.rotate_pct,
            config.checkpoint.hard_stop_pct,
            config.checkpoint.max_context_bytes,
        ),
        reservation_hints: bead.declared_paths.clone(),
        parent_handoffs,
        checkpoint: None,
        previous_failure_class: bead.last_failure_class,
        previous_outcome: None,
        rescue_card: None,
        retry_delta_summary: None,
        retrieval_query: None,
        token_budget: None,
        ordinal_in_run: 1,
        archive_bundle: None,
        playbook_rules: Vec::new(),
        env: Vec::new(),
        shutdown: SessionShutdownConfig::default(),
    }
}

/// Process unresolved mirror outbox entries, attempting to sync them to br.
pub fn process_mirror_outbox<C: BrClient>(db: &mut Database, br: &C) -> Result<()> {
    // Attempt up to 5 at a time to avoid stalling the dispatch loop indefinitely.
    let pending = db.list_pending_mirror_operations(5)
        .context("list pending mirror operations")?;
        
    for record in pending {
        db.mark_mirror_in_progress(&record.id)
            .context("mark mirror in progress")?;

        match br.mirror_handoff(&record.bead_id, &record.handoff, record.close_bead) {
            Ok(()) => {
                db.record_mirror_success(&record.id, &record.run_id)
                    .context("record mirror success")?;
                eprintln!(
                    "grove mirror: successfully synced outbox entry for {}",
                    record.bead_id.as_str()
                );
            }
            Err(error) => {
                let attempt = record.attempt_count + 1;
                // Backoff: 1m, 2m, 4m, 8m... up to 60m max.
                let backoff_mins = (1i64 << (attempt.min(6) - 1)).min(60);
                let next_retry = chrono::Utc::now() + chrono::Duration::minutes(backoff_mins);

                let error_msg = error.to_string();
                db.record_mirror_failure(
                    &record.id,
                    &record.run_id,
                    &error_msg,
                    Some(&next_retry),
                ).context("record mirror failure")?;
                
                eprintln!(
                    "grove mirror: failed to sync outbox entry for {} (attempt {}): {} (will retry after {})",
                    record.bead_id.as_str(),
                    attempt,
                    error_msg,
                    next_retry.format("%H:%M:%S")
                );
            }
        }
    }
    
    Ok(())
}

/// Run the dispatch loop: repeatedly poll for ready beads, score, pick the best,
/// dispatch a session, and repeat until exit conditions are met.
///
/// This function enforces bounded concurrency via `max_parallel`, heartbeats the
/// leader lease on each cycle, and exits on queue exhaustion, max runs, or
/// contested leader lease.
pub fn run_dispatch_loop<B: ClaudeBackend + Clone + 'static, C: BrClient>(
    db: &mut Database,
    backend: &B,
    br: &C,
    config: &GroveConfig,
    lease_config: &LeaderLeaseConfig,
    loop_config: &DispatchLoopConfig,
) -> Result<DispatchLoopOutcome> {
    let mut dispatched_count: u32 = 0;
    let mut poll_cycles: u32 = 0;
    let mut consecutive_empty_polls: u32 = 0;
    let poll_sleep = Duration::from_millis(config.scheduler.poll_interval_ms);
    let mut inflight_workers: HashMap<BeadId, InFlightWorker> = HashMap::new();
    let (completed_tx, completed_rx) = mpsc::channel::<CompletedWorker>();

    loop {
        poll_cycles += 1;

        while let Ok(completed) = completed_rx.try_recv() {
            let CompletedWorker { ctx, result } = completed;
            if let Some(worker) = inflight_workers.remove(&ctx.bead_id) {
                let _ = worker.handle.join();
            }

            dispatched_count += 1;

            match result {
                Ok(outcome) => {
                    apply_reaction_side_effects(db, config, &ctx, Some(&outcome), None);
                    if outcome.session.session.stop_reason == Some(grove_types::StopReason::Kill) {
                        let _ = db.write_event_log(
                            grove_types::EventKind::CoordinatorStopped,
                            Some(&ctx.bead_id),
                            Some(&ctx.run_id),
                            Some(&ctx.session_id),
                            &serde_json::json!({
                                "exit_reason": "shutdown signal received",
                                "stop_reason": grove_types::CoordinatorStopReason::Interrupted.as_str(),
                                "forced_termination": true,
                                "running_session_count": inflight_workers.len(),
                                "leader_released": false,
                            }),
                            &chrono::Utc::now(),
                        );
                    }
                    eprintln!(
                        "grove dispatch: {} completed with status {:?}",
                        ctx.bead_id.as_str(),
                        outcome.run.status
                    );

                    if config.reservations.enabled {
                        let _ = ReservationManager::release_for_run(
                            db,
                            &ctx.bead_id,
                            &ctx.run_id,
                            chrono::Utc::now(),
                        );
                    }

                    if outcome.run.status == grove_types::RunStatus::Succeeded {
                        if let Some(handoff) = outcome.handoff.as_ref() {
                            match br.mirror_handoff(&ctx.bead_id, handoff, true) {
                                Ok(()) => {
                                    eprintln!(
                                        "grove dispatch: mirrored {} to br",
                                        ctx.bead_id.as_str()
                                    );
                                }
                                Err(error) => {
                                    eprintln!(
                                        "grove dispatch: mirror failed for {}: {error}",
                                        ctx.bead_id.as_str()
                                    );
                                    let _ = db.enqueue_mirror_outbox(
                                        &ctx.bead_id,
                                        &ctx.run_id,
                                        handoff,
                                        true,
                                    );
                                }
                            }
                        }
                    }
                }
                Err(error) => {
                    apply_reaction_side_effects(db, config, &ctx, None, Some(&error));
                    eprintln!(
                        "grove dispatch: {} failed: {error}",
                        ctx.bead_id.as_str()
                    );
                    if config.reservations.enabled {
                        let _ = ReservationManager::release_for_run(
                            db,
                            &ctx.bead_id,
                            &ctx.run_id,
                            chrono::Utc::now(),
                        );
                    }
                }
            }

            if let Err(error) = grove_br::sync_bead_cache(br, db) {
                eprintln!("grove dispatch: bead cache sync failed: {error}");
            }
        }

        if loop_config.shutdown_signal.is_triggered() {
            if inflight_workers.is_empty() {
                eprintln!("grove dispatch: shutdown signal detected, exiting gracefully");
                let exit_reason = DispatchExitReason::ShutdownRequested;
                return Ok(DispatchLoopOutcome {
                    dispatched_count,
                    poll_cycles,
                    exit_reason: exit_reason.clone(),
                    stop_reason: exit_reason.to_stop_reason(),
                });
            }
            std::thread::sleep(poll_sleep);
            continue;
        }

        if let Some(limit) = loop_config.max_poll_cycles {
            if poll_cycles > limit {
                let exit_reason = DispatchExitReason::MaxPollCycles { limit };
                return Ok(DispatchLoopOutcome {
                    dispatched_count,
                    poll_cycles,
                    exit_reason: exit_reason.clone(),
                    stop_reason: exit_reason.to_stop_reason(),
                });
            }
        }

        if let Some(max_runs) = loop_config.max_total_runs {
            if dispatched_count >= max_runs {
                let exit_reason = DispatchExitReason::MaxRunsReached;
                return Ok(DispatchLoopOutcome {
                    dispatched_count,
                    poll_cycles,
                    exit_reason: exit_reason.clone(),
                    stop_reason: exit_reason.to_stop_reason(),
                });
            }
        }

        let now = chrono::Utc::now();
        match LeaderLeaseManager::heartbeat(db, lease_config, now)? {
            Some(_) => {}
            None => {
                let exit_reason = DispatchExitReason::LeaderContested;
                return Ok(DispatchLoopOutcome {
                    dispatched_count,
                    poll_cycles,
                    exit_reason: exit_reason.clone(),
                    stop_reason: exit_reason.to_stop_reason(),
                });
            }
        }

        if let Err(error) = process_mirror_outbox(db, br) {
            eprintln!("grove mirror: failed to process outbox: {error:#}");
        }

        if let Err(error) = crate::scoring::run_scoring_pass(db, &crate::scoring::ScoringConfig::default()) {
            eprintln!("grove playbook: scoring pass failed: {error:#}");
        }

        let ready_beads = match br.ready() {
            Ok(summaries) => summaries,
            Err(error) => {
                eprintln!("grove dispatch: br ready failed: {error}");
                std::thread::sleep(poll_sleep);
                continue;
            }
        };

        let available_slots = config.scheduler.max_parallel.saturating_sub(inflight_workers.len());
        if available_slots == 0 {
            std::thread::sleep(poll_sleep);
            continue;
        }

        if ready_beads.is_empty() {
            if inflight_workers.is_empty() {
                consecutive_empty_polls += 1;
                if consecutive_empty_polls >= 3 {
                    let exit_reason = DispatchExitReason::QueueEmpty;
                    return Ok(DispatchLoopOutcome {
                        dispatched_count,
                        poll_cycles,
                        exit_reason: exit_reason.clone(),
                        stop_reason: exit_reason.to_stop_reason(),
                    });
                }
            }
            std::thread::sleep(poll_sleep);
            continue;
        }
        consecutive_empty_polls = 0;

        let ready_ids: HashSet<BeadId> = ready_beads
            .iter()
            .map(|summary| summary.id.clone())
            .collect();
        let beads = db
            .list_bead_records()
            .context("list bead records for dispatch")?;
        let now = chrono::Utc::now();
        let mut excluded_ids: HashSet<BeadId> = inflight_workers.keys().cloned().collect();
        let mut launched_any = false;

        for _ in 0..available_slots {
            let Some(bead) = select_best_candidate_excluding(&beads, &ready_ids, &excluded_ids, config, now) else {
                break;
            };

            let bead_id = bead.bead.id.clone();
            let run_id = RunId::new(format!(
                "run-{}-{}",
                bead_id.as_str(),
                chrono::Utc::now().format("%Y%m%dT%H%M%S%3f")
            ));
            let session_id = SessionId::new(format!("ses-{}", run_id.as_str()));
            let attempt_no = db
                .list_task_runs_for_bead(&bead_id)
                .map(|runs| runs.len() as i32 + 1)
                .unwrap_or(1);

            if config.reservations.enabled && !bead.declared_paths.is_empty() {
                let expires_at = now
                    + chrono::Duration::minutes(config.reservations.default_ttl_minutes as i64);
                let requests: Vec<AcquireReservationInput> = bead
                    .declared_paths
                    .iter()
                    .map(|path| AcquireReservationInput {
                        path_pattern: path.clone(),
                        mode: ReservationMode::Exclusive,
                        reason: Some(format!("dispatch {}", bead_id.as_str())),
                        expires_at,
                    })
                    .collect();
                let outcome = ReservationManager::acquire_for_run(
                    db,
                    &bead_id,
                    Some(&run_id),
                    &requests,
                    now,
                )?;
                if !outcome.conflicts.is_empty() {
                    eprintln!(
                        "grove dispatch: skipping {} due to {} reservation conflict(s)",
                        bead_id.as_str(),
                        outcome.conflicts.len()
                    );
                    excluded_ids.insert(bead_id.clone());
                    continue;
                }
            }

            let parent_handoffs = crate::parent_handoff_summaries(db, &bead_id)
                .unwrap_or_default();
            let mut request = build_session_request(
                bead,
                config,
                &loop_config.working_dir,
                &run_id,
                &session_id,
                parent_handoffs,
            );

            let mut search_tokens: Vec<String> = bead.bead.title
                .chars()
                .map(|c| if c.is_alphanumeric() { c } else { ' ' })
                .collect::<String>()
                .split_whitespace()
                .map(|s| s.to_string())
                .collect();

            if let Some(desc) = bead.bead.description.as_deref() {
                search_tokens.extend(
                    desc.chars()
                        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
                        .collect::<String>()
                        .split_whitespace()
                        .map(|s| s.to_string())
                );
            }

            search_tokens.sort_by_key(|a| std::cmp::Reverse(a.len()));
            search_tokens.truncate(5);
            let fts_query = search_tokens.join(" OR ");

            if !fts_query.is_empty() {
                request.retrieval_query = Some(fts_query.clone());
                if let Ok(bundle) = db.search_archive_fts(&fts_query, 5) {
                    request.archive_bundle = Some(bundle);
                }
            }

            if let Ok(mut active_rules) = db.list_active_bullets(None) {
                active_rules.sort_by(|a, b| {
                    b.effective_score
                        .unwrap_or(0.0)
                        .partial_cmp(&a.effective_score.unwrap_or(0.0))
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                active_rules.truncate(5);
                request.playbook_rules = active_rules;
            }

            if let Some(failure_class) = bead.last_failure_class {
                request = request.with_retry_context(failure_class, None);
            }

            request.shutdown = SessionShutdownConfig {
                signal: Some(loop_config.shutdown_signal.shared_flag()),
                grace_period: Some(Duration::from_millis(config.scheduler.shutdown_grace_period_ms)),
            };

            eprintln!(
                "grove dispatch: dispatching {} (attempt {}) as run {}",
                bead_id.as_str(),
                attempt_no,
                run_id.as_str()
            );

            if loop_config.shutdown_signal.is_triggered() {
                let _ = db.write_event_log(
                    grove_types::EventKind::ShutdownRequested,
                    Some(&bead_id),
                    Some(&run_id),
                    Some(&session_id),
                    &serde_json::json!({
                        "signal": "ctrlc",
                        "grace_period_ms": config.scheduler.shutdown_grace_period_ms,
                    }),
                    &chrono::Utc::now(),
                );
            }

            let worker_ctx = DispatchedWorkerContext {
                bead_id: bead_id.clone(),
                run_id: run_id.clone(),
                session_id: session_id.clone(),
            };
            let worker_db_path = loop_config.db_path.clone();
            let worker_backend = backend.clone();
            let worker_tx = completed_tx.clone();
            let worker_ctx_for_thread = worker_ctx.clone();
            let handle = std::thread::spawn(move || {
                let result = (|| -> Result<PersistedTaskRunOutcome, String> {
                    let mut worker_db = Database::open(&worker_db_path)
                        .map_err(|error| error.to_string())?;
                    worker_db.migrate().map_err(|error| error.to_string())?;
                    execute_persisted_single_task_session(
                        &mut worker_db,
                        &worker_backend,
                        request,
                        attempt_no,
                    )
                    .map_err(|error| error.to_string())
                })();

                let _ = worker_tx.send(CompletedWorker {
                    ctx: worker_ctx_for_thread,
                    result,
                });
            });

            inflight_workers.insert(bead_id.clone(), InFlightWorker { handle });
            excluded_ids.insert(bead_id);
            launched_any = true;
        }

        if !launched_any {
            std::thread::sleep(poll_sleep);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grove_br::{BeadCacheStore, BrCapability, BrDependencySnapshot, BrError, BrIssueDetail, BrIssueSummary, BrVersion};
    use grove_session::CliClaudeBackend;
    use grove_types::{
        BeadId, BeadPriority, BeadRef, GroveBeadRecord, GroveBeadStatus, Timestamp,
    };
    use std::collections::HashSet;
    use std::error::Error;

    type TestResult<T = ()> = Result<T, Box<dyn Error>>;

    fn sample_bead(
        bead_id: &str,
        priority: BeadPriority,
        grove_status: GroveBeadStatus,
    ) -> TestResult<GroveBeadRecord> {
        let created_at: Timestamp = "2026-03-16T10:00:00Z".parse()?;
        let updated_at: Timestamp = "2026-03-16T11:00:00Z".parse()?;
        Ok(GroveBeadRecord {
            bead: BeadRef {
                id: BeadId::new(bead_id),
                title: format!("bead {bead_id}"),
                description: None,
                priority,
                issue_type: "task".into(),
                br_status: "open".into(),
                assignee: None,
                labels: Vec::new(),
                created_at,
                updated_at,
            },
            grove_status,
            declared_paths: Vec::new(),
            metadata: Default::default(),
            last_run_id: None,
            retry_after: None,
            last_failure_class: None,
            last_failure_detail: None,
            synced_at: updated_at,
            runtime_updated_at: updated_at,
        })
    }

    fn bead_summary(bead_id: &str, priority: BeadPriority) -> TestResult<BrIssueSummary> {
        let created_at: Timestamp = "2026-03-16T10:00:00Z".parse()?;
        let updated_at: Timestamp = "2026-03-16T11:00:00Z".parse()?;
        Ok(BrIssueSummary {
            id: BeadId::new(bead_id),
            title: format!("bead {bead_id}"),
            description: None,
            priority,
            issue_type: "task".into(),
            status: "open".into(),
            assignee: None,
            labels: Vec::new(),
            created_at,
            updated_at,
            blocked_by: Vec::new(),
            blocks: Vec::new(),
            raw_json: serde_json::json!({}),
        })
    }

    #[derive(Clone)]
    struct TestBrClient {
        ready: Vec<BrIssueSummary>,
        open: Vec<BrIssueSummary>,
    }

    impl BrClient for TestBrClient {
        fn ready(&self) -> Result<Vec<BrIssueSummary>, BrError> {
            Ok(self.ready.clone())
        }

        fn list_open(&self) -> Result<Vec<BrIssueSummary>, BrError> {
            Ok(self.open.clone())
        }

        fn show(&self, id: &BeadId) -> Result<BrIssueDetail, BrError> {
            let summary = self
                .open
                .iter()
                .find(|bead| bead.id == *id)
                .cloned()
                .ok_or_else(|| BrError::BeadNotFound { id: id.clone() })?;
            Ok(BrIssueDetail {
                summary,
                closed_at: None,
                close_reason: None,
                comments: Vec::new(),
                metadata: serde_json::json!({}),
            })
        }

        fn dep_list(&self, id: &BeadId) -> Result<BrDependencySnapshot, BrError> {
            Ok(BrDependencySnapshot {
                bead_id: id.clone(),
                blocked_by: Vec::new(),
                blocks: Vec::new(),
                rows: Vec::new(),
            })
        }

        fn capability(&self) -> Result<BrCapability, BrError> {
            Ok(BrCapability {
                available: true,
                version_line: Some("br test".to_owned()),
                version: Some(BrVersion {
                    raw: "br test".to_owned(),
                    major: Some(0),
                    minor: Some(1),
                    patch: Some(0),
                }),
                beads_dir_exists: true,
            })
        }

        fn close_bead(&self, _id: &BeadId, _reason: Option<&str>) -> Result<(), BrError> {
            Ok(())
        }

        fn add_comment(&self, _id: &BeadId, _text: &str) -> Result<(), BrError> {
            Ok(())
        }

        fn mirror_handoff(
            &self,
            _id: &BeadId,
            _handoff: &grove_types::HandoffRecord,
            _close_bead: bool,
        ) -> Result<(), BrError> {
            Ok(())
        }
    }

    #[test]
    fn select_best_candidate_picks_highest_priority() -> TestResult {
        let config = GroveConfig::default();
        let beads = vec![
            sample_bead("grove-low", BeadPriority::P3, GroveBeadStatus::Ready)?,
            sample_bead("grove-high", BeadPriority::P0, GroveBeadStatus::Ready)?,
            sample_bead("grove-mid", BeadPriority::P1, GroveBeadStatus::Ready)?,
        ];
        let ready_ids: HashSet<BeadId> = beads.iter().map(|b| b.bead.id.clone()).collect();
        let now: Timestamp = "2026-03-16T12:00:00Z".parse()?;

        let result = select_best_candidate(&beads, &ready_ids, &config, now);
        assert_eq!(
            result.map(|b| b.bead.id.as_str()),
            Some("grove-high")
        );
        Ok(())
    }

    #[test]
    fn select_best_candidate_skips_running_beads() -> TestResult {
        let config = GroveConfig::default();
        let beads = vec![
            sample_bead("grove-running", BeadPriority::P0, GroveBeadStatus::Running)?,
            sample_bead("grove-ready", BeadPriority::P1, GroveBeadStatus::Ready)?,
        ];
        let ready_ids: HashSet<BeadId> = beads.iter().map(|b| b.bead.id.clone()).collect();
        let now: Timestamp = "2026-03-16T12:00:00Z".parse()?;

        let result = select_best_candidate(&beads, &ready_ids, &config, now);
        assert_eq!(
            result.map(|b| b.bead.id.as_str()),
            Some("grove-ready")
        );
        Ok(())
    }

    #[test]
    fn select_best_candidate_returns_none_when_no_eligible() -> TestResult {
        let config = GroveConfig::default();
        let beads = vec![
            sample_bead("grove-running", BeadPriority::P0, GroveBeadStatus::Running)?,
            sample_bead("grove-succeeded", BeadPriority::P1, GroveBeadStatus::Succeeded)?,
        ];
        let ready_ids: HashSet<BeadId> = beads.iter().map(|b| b.bead.id.clone()).collect();
        let now: Timestamp = "2026-03-16T12:00:00Z".parse()?;

        let result = select_best_candidate(&beads, &ready_ids, &config, now);
        assert!(result.is_none());
        Ok(())
    }

    #[test]
    fn select_best_candidate_only_considers_br_ready_beads() -> TestResult {
        let config = GroveConfig::default();
        let beads = vec![
            sample_bead("grove-ready-both", BeadPriority::P2, GroveBeadStatus::Ready)?,
            sample_bead("grove-ready-local-only", BeadPriority::P0, GroveBeadStatus::Ready)?,
        ];
        // Only the P2 bead is in the br ready set.
        let mut ready_ids = HashSet::new();
        ready_ids.insert(BeadId::new("grove-ready-both"));
        let now: Timestamp = "2026-03-16T12:00:00Z".parse()?;

        let result = select_best_candidate(&beads, &ready_ids, &config, now);
        assert_eq!(
            result.map(|b| b.bead.id.as_str()),
            Some("grove-ready-both")
        );
        Ok(())
    }

    #[test]
    fn select_best_candidate_excluding_skips_inflight_beads() -> TestResult {
        let config = GroveConfig::default();
        let beads = vec![
            sample_bead("grove-p0", BeadPriority::P0, GroveBeadStatus::Ready)?,
            sample_bead("grove-p1", BeadPriority::P1, GroveBeadStatus::Ready)?,
            sample_bead("grove-p2", BeadPriority::P2, GroveBeadStatus::Ready)?,
        ];
        let ready_ids: HashSet<BeadId> = beads.iter().map(|b| b.bead.id.clone()).collect();
        let excluded_ids = HashSet::from([BeadId::new("grove-p0")]);
        let now: Timestamp = "2026-03-16T12:00:00Z".parse()?;

        let result = select_best_candidate_excluding(&beads, &ready_ids, &excluded_ids, &config, now);
        assert_eq!(result.map(|b| b.bead.id.as_str()), Some("grove-p1"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn dispatch_loop_persists_multiple_inflight_runs_up_to_parallel_limit() -> TestResult {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use tempfile::tempdir;

        let dir = tempdir()?;
        let workspace_dir = dir.path().join("workspace");
        fs::create_dir_all(workspace_dir.join(".grove"))?;
        let workspace_dir = Utf8PathBuf::from_path_buf(workspace_dir)
            .map_err(|_| std::io::Error::other("workspace dir must be valid UTF-8"))?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;

        let mut db = Database::open(&db_path)?;
        db.migrate()?;
        db.upsert_bead_cache(&bead_summary("grove-a", BeadPriority::P0)?)?;
        db.upsert_bead_cache(&bead_summary("grove-b", BeadPriority::P1)?)?;
        db.set_grove_status(&BeadId::new("grove-a"), GroveBeadStatus::Ready)?;
        db.set_grove_status(&BeadId::new("grove-b"), GroveBeadStatus::Ready)?;

        let script_path = dir.path().join("sleepy-claude");
        fs::write(
            &script_path,
            "#!/bin/sh\nsleep 0.2\nprintf 'GROVE_RESULT: ok\\nGROVE_ARTIFACTS: [\"src/lib.rs\"]\\nGROVE_EXIT: true\\nall tasks complete\\nimplementation complete\\n'\n",
        )?;
        let mut permissions = fs::metadata(&script_path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions)?;

        let backend = CliClaudeBackend::new(script_path.to_string_lossy().into_owned());
        let br = TestBrClient {
            ready: vec![
                bead_summary("grove-a", BeadPriority::P0)?,
                bead_summary("grove-b", BeadPriority::P1)?,
            ],
            open: vec![
                bead_summary("grove-a", BeadPriority::P0)?,
                bead_summary("grove-b", BeadPriority::P1)?,
            ],
        };
        let mut config = GroveConfig::default();
        config.scheduler.max_parallel = 2;
        config.scheduler.poll_interval_ms = 10;
        let lease_config = LeaderLeaseConfig {
            owner_label: "test-owner".to_owned(),
            lease_ttl: chrono::Duration::seconds(1),
        };
        let now = chrono::Utc::now();
        let _ = LeaderLeaseManager::acquire(&mut db, &lease_config, None, now)?;
        let loop_config = DispatchLoopConfig {
            max_total_runs: Some(2),
            max_poll_cycles: Some(50),
            working_dir: workspace_dir,
            shutdown_signal: ShutdownSignal::new(),
            db_path,
        };

        let outcome = run_dispatch_loop(&mut db, &backend, &br, &config, &lease_config, &loop_config)?;
        assert_eq!(outcome.dispatched_count, 2);

        let runs = db.connection().query_row(
            "SELECT COUNT(*) FROM task_runs WHERE bead_id IN ('grove-a', 'grove-b')",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        assert_eq!(runs, 2);
        Ok(())
    }

    #[test]
    fn dispatch_exit_reason_display() {
        assert_eq!(
            DispatchExitReason::QueueEmpty.to_string(),
            "no dispatchable beads remain"
        );
        assert_eq!(
            DispatchExitReason::MaxRunsReached.to_string(),
            "reached max total runs"
        );
        assert_eq!(
            DispatchExitReason::LeaderContested.to_string(),
            "leader lease contested"
        );
        assert_eq!(
            DispatchExitReason::MaxPollCycles { limit: 100 }.to_string(),
            "exceeded max poll cycles (100)"
        );
    }

    #[test]
    fn score_bead_applies_retry_penalty() -> TestResult {
        let config = GroveConfig::default();
        let ready = sample_bead("grove-ready", BeadPriority::P1, GroveBeadStatus::Ready)?;
        let retrying = sample_bead(
            "grove-retrying",
            BeadPriority::P1,
            GroveBeadStatus::WaitingToRetry,
        )?;

        let score_ready = score_bead(&ready, &config);
        let score_retrying = score_bead(&retrying, &config);

        assert!(
            score_ready > score_retrying,
            "ready ({score_ready}) should outscore retrying ({score_retrying})"
        );
        assert!(
            (score_ready - score_retrying - f64::from(config.scheduler.retry_penalty)).abs() < 0.01
        );
        Ok(())
    }
}
