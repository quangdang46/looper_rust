# Grove Release-Readiness Criteria

**Document ID:** `docs/release-criteria.md`
**Bead:** `grove-1j9.4`
**Last updated:** 2026-03-22
**Phase gate:** All Phase 3–6 acceptance tasks closed

---

## Purpose

This document defines what "safe to leave unattended" means for Grove. It captures explicit, verifiable criteria across three axes:

1. **Crash Safety** — Grove survives failure and resumes correctly
2. **Observability** — Operators can inspect, diagnose, and reconstruct what happened
3. **Operator Trust** — The system is predictable, bounded, and recoverable

This document is the source of truth for release gates. Future hardening work is judged against these criteria, not feature count.

---

## Definitions

### Operational Guarantees

| Term | Meaning |
|------|---------|
| **Crash safety** | Grove survives coordinator crashes, session failures, and mirror outages without losing bead state or leaving tasks in ambiguous limbo |
| **Observability** | Every meaningful state transition is recorded in the event log and readable via `grove inspect` or `grove log` |
| **Operator trust** | An operator who walks away and returns hours later can reconstruct what happened, why, and what to do next |

### Key State Types

| Type | Description |
|------|-------------|
| `RunStatus` | `Active`, `WaitingToRetry`, `Checkpointed`, `Succeeded`, `Failed` |
| `MirrorStatus` | `Pending`, `InProgress`, `Succeeded`, `Failed` |
| `CoordinatorStopReason` | `UserStopped`, `Interrupted`, `QueueEmpty`, `MaxRunsReached`, `LeaderContested`, `MaxPollCycles`, `InternalError` |
| `EscalationTier` | `FirstAttempt`, `SecondAttempt`, `ThirdAttempt`, `FinalAttempt`, `GiveUp` |
| `FailureClass` | `Timeout`, `RateLimit`, `PermissionDenied`, `CircuitOpen`, `NoProgress`, `RepeatedError`, `ProtocolMalformed`, `ClaudeCrashed`, `BrMirrorFailed`, `Interrupted`, `Unknown` |
| `RecoveryCapsuleOutcome` | `Failed`, `Interrupted`, `Checkpointed` |

---

## I. Crash Safety Criteria

### C1: Coordinator Crash Recovery

**Criterion:** When the coordinator process is killed mid-execution and restarted, all active runs are reconciled to a known state.

**Evidence:**
- `test_interrupted_run_reconciliation_marks_active_runs_failed` (phase3 acceptance)
- `db.reconcile_interrupted_runs()` transitions `Active` runs to `Failed` with `FailureClass::Interrupted`
- `RecoveryActionTaken` event logged on reconciliation

**Verification:**
```
grove run &
kill -9 <pid>
grove run
grove status
# All previously-active runs should appear as Failed (interrupted), not stuck in Active
grove inspect <bead-id>
# Should show recovery capsule with outcome=Interrupted
```

### C2: Session Failure with Escalation Tiers

**Criterion:** Failed sessions escalate through `EscalationTier` with distinct mutation strategies. The progression is bounded (max 5 tiers) and ends in `GiveUp` with a recovery capsule.

**Evidence:**
- `EscalationTier` enum: `FirstAttempt → SecondAttempt → ThirdAttempt → FinalAttempt → GiveUp`
- Tier progression tested in `lib.rs` compile-time assertions
- `EscalationTierChanged` and `EscalationTierReset` events in event schema
- Phase 2 acceptance tests exercise tier escalation

**Mutation strategy by tier:**
| Tier | Strategy |
|------|----------|
| FirstAttempt | None |
| SecondAttempt | Rescue prompt injection |
| ThirdAttempt | `NarrowClaimedPaths` mutation |
| FinalAttempt | `SwitchModel` mutation |
| GiveUp | Recovery capsule, no further attempt |

### C3: Circuit Breaker

**Criterion:** Stuck loops are detected and the session is terminated before indefinite spinning.

**Evidence:**
- `CircuitBreakerState` with `CircuitState`: `Closed → Open → HalfOpen → Closed`
- Thresholds: `no_progress_threshold=3`, `same_error_threshold=5`, `permission_denial_threshold=2`
- `circuit_breaker_state` persisted in `TaskRunRecord`
- Phase 2 acceptance tests exercise circuit breaker transitions

**Verification:**
```
grove inspect <bead-id>
# Should show circuit_breaker_state
```

### C4: Context Exhaustion — Checkpoint and Resume

**Criterion:** When Claude's context approaches the configured threshold, Grove emits `GROVE_CHECKPOINT`, persists it, and spawns a new session that resumes from the checkpoint.

**Evidence:**
- Checkpoint rotation thresholds: `warn_pct=0.70`, `rotate_pct=0.82`, `hard_stop_pct=0.90`
- `GROVE_CHECKPOINT` protocol marker with `progress`, `next_step`, `context`, `open_questions`, `claimed_paths`
- `checkpoint_count` tracked in `TaskRunRecord`
- Checkpoint files persisted in `.grove/checkpoints/<bead-id>/`
- Phase 2 acceptance tests cover checkpoint emission and persistence

**Verification:**
```
# Session outputs: GROVE_CHECKPOINT: {"progress":"60%","next_step":"finish auth"}
# Checkpoint file appears in .grove/checkpoints/<bead-id>/
# New session resumes with checkpoint injected
```

### C5: Mirror Pending Durability

**Criterion:** When `br` mirror fails, the run is marked `Succeeded` locally and the mirror action is queued as `mirror-pending`. The outbox is durable and retried.

**Evidence:**
- `MirrorStatus::Pending → InProgress → Succeeded/Failed` state machine
- `MirrorOutboxRecord` in schema with `attempt_count`, `last_attempt_at`, `next_retry_after`, `last_error`
- `BrMirrorFailed` is a first-class `FailureClass`
- Phase 3 acceptance covers mirror-pending behavior

**Verification:**
```
grove status
# Should show mirror-pending runs separately
# Mirror retries on next grove run
```

### C6: Graceful Shutdown

**Criterion:** Coordinator shutdown (SIGINT/SIGTERM) stops dispatch cleanly, releases the leader lease, and persists the stop reason.

**Evidence:**
- `ShutdownSignal` translates `DispatchExitReason::ShutdownRequested` → `CoordinatorStopReason::UserStopped`
- `CoordinatorStopped` event written to event log with `forced_termination`, `running_session_count`, `leader_released`
- `test_shutdown_signal_translates_to_durable_stop_reason` (phase3)
- `test_coordinator_shutdown_events_round_trip_in_db` (phase3)
- Stop reasons classified as `is_clean()` vs crash (`Interrupted`, `InternalError`, `LeaderContested`)

---

## II. Observability Criteria

### O1: Live Activity Transitions

**Criterion:** Session activity (`Idle`, `Blocked`, `Ready`, `Active`) is observable in real-time via hooks and persisted to the event log.

**Evidence:**
- `AgentActivity` enum: `Idle`, `Blocked`, `Ready`, `Active`, `Error`
- `ActivityRecordingHooks` for in-test observation
- `test_live_session_hooks_expose_idle_blocked_and_ready_activity_transitions` (phase3)
- Activity transitions tagged with detail source: `protocol_event`, `stderr`, `stream_timeout`
- `ActivityStateChanged` events in event log with `activity` and `detail` fields

**Verification:**
```
grove inspect <bead-id>
# Should show current activity state and last_activity_at
```

### O2: Run Metrics Aggregation

**Criterion:** Completed runs expose `RunMetrics` and `RunReport` for post-mortem analysis.

**Evidence:**
- `RunMetrics`: `total_duration_secs`, `checkpoints_taken`, `retries_attempted`, `rescue_injections`, `reactions_invoked`, `max_escalation_tier`, `termination_reason`
- `RunReport`: run metadata + metrics + failure class + recovery capsule + event count + timestamps
- `db.aggregate_run_metrics()` and `db.generate_run_report()` in DB layer
- `test_run_metrics_aggregation_includes_checkpoints_and_events` (phase3)
- `test_failed_run_report_includes_failure_class_and_duration` (phase3)

**Verification:**
```
grove inspect <bead-id>
# Should show run report with metrics, event count, failure class
```

### O3: Stop Reason Explainability

**Criterion:** Every coordinator stop has a durable, human-readable `CoordinatorStopReason` recorded in the event log.

**Evidence:**
- `CoordinatorStopReason` enum with 7 variants, each with `as_str()` and `Display` implementations
- `is_user_initiated()` and `is_clean()` predicates
- `test_empty_queue_maps_to_clean_stop_reason` (phase3)
- `test_leader_contested_maps_to_uncle_fast_fail_reason` (phase3)

**Verification:**
```
grove status
grove log <bead-id>
# Should show CoordinatorStopped event with stop_reason
```

### O4: Prompt Provenance and Dispatch Decisions

**Criterion:** Every dispatched session has a recorded prompt manifest showing what was injected and why.

**Evidence:**
- `PromptManifest` type with prompt template, injected context, provenance tags
- Dispatch decision reason stored with the session record
- `grove inspect <bead-id>` surfaces the prompt manifest
- Phase 1 acceptance covers prompt assembly and injection

### O5: Config Provenance

**Criterion:** Config snapshots are persisted at coordinator start so the config state at any run is auditable.

**Evidence:**
- `grove.toml` schema documented in `grove-config`
- Config snapshot stored in `.grove/config.snapshot.json`
- `grove config get` shows effective config
- Phase 1 acceptance covers config loading and defaults

### O6: Recovery Capsule for Failed/Interrupted/Checkpointed Runs

**Criterion:** Every non-successful outcome produces a typed `RecoveryCapsule` with summary, evidence, root causes, risky paths, and next-step guidance.

**Evidence:**
- `RecoveryCapsule` struct with `outcome`, `summary`, `strongest_evidence`, `likely_root_causes`, `risky_paths`, `do_not_repeat`, `next_attempt_contract`, `retry_delta_summary`, `checkpoint_progress`, `checkpoint_next_step`, `artifacts`
- `RecoveryCapsuleOutcome`: `Failed`, `Interrupted`, `Checkpointed`
- `test_interrupted_run_reconciliation_marks_active_runs_failed` covers `Interrupted` outcome
- Phase 2 acceptance covers `Checkpointed` recovery capsules

**Verification:**
```
grove inspect <bead-id>
# Should show recovery_capsule with outcome, summary, root causes, next step
grove retry <bead-id>
# Should use recovery capsule to inform next attempt
```

---

## III. Operator Trust Criteria

### T1: Reservation Safety

**Criterion:** File reservations prevent concurrent agents from editing the same files. Exclusive reservations are enforced by the coordinator before dispatch.

**Evidence:**
- `FileReservation` type with `path`, `exclusive`, `reason`, `ttl_seconds`
- `file_reservation_paths()` in MCP Agent Mail
- Reservation conflicts cause `FILE_RESERVATION_CONFLICT` errors
- `grove-24q` (closed) fixed path resolution for prompt manifest in reserved paths

**Verification:**
```
# Agent A reserves src/**/*.rs
# Agent B tries to reserve src/**/*.rs (exclusive)
# B should get FILE_RESERVATION_CONFLICT
```

### T2: Leader Lease

**Criterion:** Only one coordinator runs at a time per workspace. Leader lease prevents split-brain orchestration.

**Evidence:**
- `LeaderLeaseRecord` in schema with `acquired_at`, `released_at`
- `LeaderContested` as a `CoordinatorStopReason`
- `grove status` shows leader state

### T3: Mirror-Pending Visibility

**Criterion:** Operators can see runs that succeeded locally but haven't mirrored to `br` yet.

**Evidence:**
- `MirrorStatus::Pending` visible via `grove status`
- `MirrorOutboxRecord` queryable in DB
- Phase 3 acceptance tests cover mirror-pending lifecycle

**Verification:**
```
grove status
# Should list mirror-pending beads separately
```

### T4: Recovery and Retry

**Criterion:** Operators can retry failed or interrupted beads with the recovery capsule informing the next attempt.

**Evidence:**
- `grove retry <bead-id>` resets bead to retryable state
- Recovery capsule's `next_attempt_contract` and `retry_delta_summary` injected into next prompt
- `checkpoint_next_step` propagates to new session

**Verification:**
```
grove retry <bead-id>
grove run
# New session should receive recovery context
```

### T5: Archive and Playbook Explainability

**Criterion:** The archive and playbook are inspectable, and playbook bullet selection is explainable.

**Evidence:**
- Phase 4 acceptance: transcript searchability, retrieval-assisted prompts with bounded budgets
- Phase 5 acceptance: evidence-based promotion, weak-candidate non-promotion, bounded verification
- Phase 6 acceptance: compaction, anti-pattern inversion, explainable curation
- Playbook bullet maturity: `Candidate → Established → Proven`
- Anti-pattern inversion: harmful rules demoted with explicit reason

**Verification:**
```
grove inspect <bead-id>
# Should show which playbook bullets were selected and why
# Archive snippets should show provenance (session ID, bead ID)
```

---

## IV. Acceptance Test Coverage Summary

| Phase | Test File | Tests | Key Coverage |
|-------|-----------|-------|-------------|
| 1 | `phase1_acceptance.rs` | 29 | `grove init`, config, schema, CLI commands |
| 2 | `phase2_acceptance.rs` | 14 | Exit gate, circuit breaker, checkpoint/resume, escalation tiers, recovery capsules |
| 3 | `phase3_acceptance.rs` | 9 | Graceful shutdown, activity hooks, run metrics, failed run reports, interrupted run reconciliation, observability |
| 4 | `phase4_acceptance.rs` | 7 | Archive ingest, FTS search, retrieval-assisted prompts, provenance |
| 5 | `phase5_acceptance.rs` | 4 | Playbook bullet ingestion, evidence gates, verification-before-close |
| 6 | `phase6_acceptance.rs` | 5 | Run diaries, deduplication, anti-pattern inversion, curation explainability |

**Total acceptance tests: 68**

---

## V. Release Gates

A Grove release is considered production-ready when ALL of the following are true:

### Crash Safety Gates

- [ ] `test_interrupted_run_reconciliation_marks_active_runs_failed` passes
- [ ] `test_shutdown_signal_translates_to_durable_stop_reason` passes
- [ ] `test_coordinator_shutdown_events_round_trip_in_db` passes
- [ ] Circuit breaker state transitions (Closed/Open/HalfOpen) tested
- [ ] Escalation tier progression from FirstAttempt through GiveUp tested
- [ ] Checkpoint emission and persistence tested
- [ ] Mirror-pending outbox durability tested

### Observability Gates

- [ ] `test_live_session_hooks_expose_idle_blocked_and_ready_activity_transitions` passes
- [ ] `test_run_metrics_aggregation_includes_checkpoints_and_events` passes
- [ ] `test_failed_run_report_includes_failure_class_and_duration` passes
- [ ] `grove inspect` surfaces recovery capsule, run metrics, and event log
- [ ] `grove log` shows transcript tail and latest checkpoint
- [ ] `grove status` shows leader lease state, running sessions, mirror-pending beads

### Operator Trust Gates

- [ ] File reservation conflicts are enforced before dispatch
- [ ] Leader lease prevents concurrent coordinators
- [ ] `grove retry` resets failed beads and injects recovery capsule context
- [ ] Archive retrieval is bounded and provenance-tracked in prompt manifest
- [ ] Playbook bullets have maturity levels and can be inverted to anti-patterns
- [ ] All Phase 3–6 acceptance tests pass (68 tests total)

### Quality Gates

- [ ] `cargo check --workspace` passes
- [ ] `cargo clippy --workspace` passes with no warnings
- [ ] `cargo fmt --check` passes
- [ ] No breaking changes to the `grove.toml` schema

---

## VI. Known Gaps and Future Work

These are not release blockers but are tracked for post-MVP hardening:

| Gap | Description | Priority |
|-----|-------------|----------|
| Safety guard pattern registry | Destructive pattern detection in transcripts (documented in PLAN.md §31) | P2 |
| Run diary generation | Rich narrative from run outcomes (Phase 6) | P2 |
| Reaction evaluations on failure paths | Feedback signals from operator reactions | P2 |
| Cross-phase execution enhancements | Bead-contract-aware prompt/retry/verification behavior | P2 |

---

## VII. Interpreting This Document

**For operators:** Run `grove status`, `grove inspect <bead-id>`, and `grove log <bead-id>` to evaluate the state of any bead. All observable state is in SQLite or the event log — no shell scraping required.

**For agents:** Before closing any bead related to safety, observability, or trust, verify the corresponding gate in Section V. If a gate is red, file a new issue and link it to `grove-1j9.4` as a blocking gap.

**For future sessions:** When planning hardening work, start from the Known Gaps table (Section VI) and the Release Gates (Section V). Do not treat this document as complete — update it as the system matures.
