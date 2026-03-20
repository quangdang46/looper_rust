use crate::{
    AcquireReservationInput, DispatchEligibilityContext, LeaderLeaseConfig, LeaderLeaseManager,
    ReservationManager, execute_persisted_single_task_session,
};
use anyhow::{Context, Result, bail};
use grove_br::BrClient;
use grove_bv::BvClient;
use grove_config::GroveConfig;
use grove_db::Database;
use grove_session::{
    ClaudeBackend, ContextMonitor, ExitPolicy, SingleTaskSessionRequest,
};
use grove_types::{
    BeadId, CircuitState, ExecutionContract, GroveBeadRecord, GroveBeadStatus, PromptId,
    ReservationMode, RunId, SessionId, Timestamp,
};
use std::collections::HashSet;
use std::time::Duration;

/// Outcome of a single dispatch loop run.
#[derive(Debug, Clone)]
pub struct DispatchLoopOutcome {
    /// Total number of dispatches completed in this loop run.
    pub dispatched_count: u32,
    /// Total number of poll cycles executed.
    pub poll_cycles: u32,
    /// Reason the dispatch loop terminated.
    pub exit_reason: DispatchExitReason,
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
}

impl std::fmt::Display for DispatchExitReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QueueEmpty => write!(f, "no dispatchable beads remain"),
            Self::MaxRunsReached => write!(f, "reached max total runs"),
            Self::LeaderContested => write!(f, "leader lease contested"),
            Self::MaxPollCycles { limit } => write!(f, "exceeded max poll cycles ({limit})"),
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
    pub working_dir: camino::Utf8PathBuf,
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

/// Count beads currently in `Running` status.
fn count_running_beads(db: &Database) -> Result<usize> {
    let beads = db.list_bead_records().context("list beads for concurrency check")?;
    Ok(beads
        .iter()
        .filter(|bead| bead.grove_status == GroveBeadStatus::Running)
        .count())
}

/// Select the highest-scored dispatchable bead from the ready list.
fn select_best_candidate<'a>(
    beads: &'a [GroveBeadRecord],
    ready_ids: &HashSet<BeadId>,
    config: &GroveConfig,
    now: Timestamp,
) -> Option<&'a GroveBeadRecord> {
    let mut candidates: Vec<_> = beads
        .iter()
        .filter(|bead| {
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
    working_dir: &camino::Utf8Path,
    run_id: &RunId,
    session_id: &SessionId,
    parent_handoffs: Vec<String>,
) -> SingleTaskSessionRequest {
    let prompt_id = PromptId::new(format!("prompt-{}", run_id.as_str()));
    let transcript_path = camino::Utf8PathBuf::from(format!(
        ".grove/transcripts/{}/{}.jsonl",
        bead.bead.id.as_str(),
        session_id.as_str()
    ));
    let prompt_manifest_path = camino::Utf8PathBuf::from(format!(
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
        exit_policy: ExitPolicy::new(
            config.exit_policy.completion_indicator_threshold,
            config.exit_policy.heuristic_window,
            config.exit_policy.require_explicit_exit,
        ),
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
        token_budget: None,
        ordinal_in_run: 1,
        env: Vec::new(),
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
pub fn run_dispatch_loop<B: ClaudeBackend, C: BrClient>(
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

    loop {
        poll_cycles += 1;

        // Check max poll cycles.
        if let Some(limit) = loop_config.max_poll_cycles {
            if poll_cycles > limit {
                return Ok(DispatchLoopOutcome {
                    dispatched_count,
                    poll_cycles,
                    exit_reason: DispatchExitReason::MaxPollCycles { limit },
                });
            }
        }

        // Check max total runs.
        if let Some(max_runs) = loop_config.max_total_runs {
            if dispatched_count >= max_runs {
                return Ok(DispatchLoopOutcome {
                    dispatched_count,
                    poll_cycles,
                    exit_reason: DispatchExitReason::MaxRunsReached,
                });
            }
        }

        // Heartbeat the leader lease.
        let now = chrono::Utc::now();
        match LeaderLeaseManager::heartbeat(db, lease_config, now)? {
            Some(_) => {}
            None => {
                return Ok(DispatchLoopOutcome {
                    dispatched_count,
                    poll_cycles,
                    exit_reason: DispatchExitReason::LeaderContested,
                });
            }
        }

        // Process pending mirror outbox entries.
        if let Err(error) = process_mirror_outbox(db, br) {
            eprintln!("grove mirror: failed to process outbox: {error:#}");
        }

        // Enforce bounded concurrency.
        let running_count = count_running_beads(db)?;
        if running_count >= config.scheduler.max_parallel {
            // At capacity — sleep and retry.
            std::thread::sleep(poll_sleep);
            continue;
        }

        // Poll br for ready beads.
        let ready_beads = match br.ready() {
            Ok(summaries) => summaries,
            Err(error) => {
                eprintln!("grove dispatch: br ready failed: {error}");
                std::thread::sleep(poll_sleep);
                continue;
            }
        };

        if ready_beads.is_empty() {
            consecutive_empty_polls += 1;
            // After 3 consecutive empty polls, exit.
            if consecutive_empty_polls >= 3 {
                return Ok(DispatchLoopOutcome {
                    dispatched_count,
                    poll_cycles,
                    exit_reason: DispatchExitReason::QueueEmpty,
                });
            }
            std::thread::sleep(poll_sleep);
            continue;
        }
        consecutive_empty_polls = 0;

        let ready_ids: HashSet<BeadId> = ready_beads
            .iter()
            .map(|summary| summary.id.clone())
            .collect();

        // Load all bead records from the local cache.
        let beads = db
            .list_bead_records()
            .context("list bead records for dispatch")?;

        // Select the best candidate.
        let now = chrono::Utc::now();
        let candidate = select_best_candidate(&beads, &ready_ids, config, now);

        let Some(bead) = candidate else {
            // br says beads are ready but none pass local dispatch eligibility.
            consecutive_empty_polls += 1;
            if consecutive_empty_polls >= 3 {
                return Ok(DispatchLoopOutcome {
                    dispatched_count,
                    poll_cycles,
                    exit_reason: DispatchExitReason::QueueEmpty,
                });
            }
            std::thread::sleep(poll_sleep);
            continue;
        };

        let bead_id = bead.bead.id.clone();
        let run_id = RunId::new(format!(
            "run-{}-{}",
            bead_id.as_str(),
            now.format("%Y%m%dT%H%M%S")
        ));
        let session_id = SessionId::new(format!("ses-{}", run_id.as_str()));
        let attempt_no = db
            .list_task_runs_for_bead(&bead_id)
            .map(|runs| runs.len() as i32 + 1)
            .unwrap_or(1);

        // Acquire reservations if enabled.
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
            let outcome =
                ReservationManager::acquire_for_run(db, &bead_id, Some(&run_id), &requests, now)?;
            if !outcome.conflicts.is_empty() {
                eprintln!(
                    "grove dispatch: skipping {} due to {} reservation conflict(s)",
                    bead_id.as_str(),
                    outcome.conflicts.len()
                );
                std::thread::sleep(poll_sleep);
                continue;
            }
        }

        // Load parent handoff context.
        let parent_handoffs = crate::parent_handoff_summaries(db, &bead_id)
            .unwrap_or_default();

        // Build the session request.
        let mut request = build_session_request(
            bead,
            config,
            &loop_config.working_dir,
            &run_id,
            &session_id,
            parent_handoffs,
        );

        if let Some(failure_class) = bead.last_failure_class {
            request = request.with_retry_context(failure_class, None);
        }

        // Execute the session.
        eprintln!(
            "grove dispatch: dispatching {} (attempt {}) as run {}",
            bead_id.as_str(),
            attempt_no,
            run_id.as_str()
        );

        match execute_persisted_single_task_session(db, backend, request, attempt_no) {
            Ok(outcome) => {
                dispatched_count += 1;
                eprintln!(
                    "grove dispatch: {} completed with status {:?}",
                    bead_id.as_str(),
                    outcome.run.status
                );

                // Release reservations after completion.
                if config.reservations.enabled {
                    let _ = ReservationManager::release_for_run(
                        db,
                        &bead_id,
                        &run_id,
                        chrono::Utc::now(),
                    );
                }

                // Attempt mirror to br (grove-1j9.7.6 plumbing).
                if outcome.run.status == grove_types::RunStatus::Succeeded {
                    if let Some(handoff) = outcome.handoff.as_ref() {
                        match br.mirror_handoff(&bead_id, handoff, true) {
                            Ok(()) => {
                                eprintln!(
                                    "grove dispatch: mirrored {} to br",
                                    bead_id.as_str()
                                );
                            }
                            Err(error) => {
                                eprintln!(
                                    "grove dispatch: mirror failed for {}: {error}",
                                    bead_id.as_str()
                                );
                                // Write to mirror outbox for later retry.
                                let _ = db.enqueue_mirror_outbox(
                                    &bead_id,
                                    &run_id,
                                    &handoff,
                                    true,
                                );
                            }
                        }
                    }
                }
            }
            Err(error) => {
                dispatched_count += 1;
                eprintln!(
                    "grove dispatch: {} failed: {error}",
                    bead_id.as_str()
                );

                // Release reservations after failure.
                if config.reservations.enabled {
                    let _ = ReservationManager::release_for_run(
                        db,
                        &bead_id,
                        &run_id,
                        chrono::Utc::now(),
                    );
                }
            }
        }

        // Re-sync the bead cache after dispatch.
        if let Err(error) = grove_br::sync_bead_cache(br, db) {
            eprintln!("grove dispatch: bead cache sync failed: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grove_types::{
        BeadId, BeadPriority, BeadRef, GroveBeadRecord, GroveBeadStatus, RunId, Timestamp,
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
