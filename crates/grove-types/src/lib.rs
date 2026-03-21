pub mod archive;
mod checkpoint;
mod errors;
mod event;
mod handoff;
mod ids;
mod ops;
pub mod playbook;
mod priority;
pub mod prompt;
pub mod reaction;
mod reservation;
mod run;
mod session;
mod task;
mod time;

pub use archive::{
    ConversationRecord, MessageRecord, MessageRole, RelevantSnippet, RetrievalBundle,
    SnippetRecord, SourceRecord,
};
pub use checkpoint::{
    CheckpointPayload, CheckpointRecord, ProtocolEvent, ProtocolState, ResumeGeneration,
};
pub use errors::{GroveTypesError, InvalidTransition};
pub use event::{
    ContextSnapshot, EventError, EventKind, EventLogRecord, EventOutcome, RunMetrics, RunReport,
};
pub use handoff::HandoffRecord;
pub use ids::{BeadId, BulletId, CheckpointId, PromptId, RunId, SessionId, SourceId, TickId};
pub use ops::{
    ConfigSnapshotRecord, DispatchDecisionRecord, IntegrityCheckRecord, PromptMaterializationRecord,
};
pub use playbook::{
    BulletMaturity, BulletScope, BulletState, BulletType, FeedbackEventRecord, FeedbackKind,
    MemoryDiaryRecord, PlaybookBulletRecord,
};
pub use priority::BeadPriority;
pub use prompt::{
    ExecutionContract, PromptManifest, PromptManifestSection, PromptSectionProvenance,
    PromptSegment, PromptSegmentKind, PromptTrimReason,
};
pub use reaction::{
    MutationStrategy, ReactionAction, ReactionContextSnapshot, ReactionOutcome, ReactionRecord,
    ReactionRule, ReactionTrigger, default_reactions,
};
pub use reservation::{ReservationConflict, ReservationMode, ReservationRecord};
pub use run::{
    AgentActivity, AutonomousAction, CoordinatorStopReason, EscalationPolicy, EscalationTier,
    FailureClass, LeaderLeaseRecord, MirrorOutboxRecord, MirrorStatus, RecoveryCapsule,
    RecoveryCapsuleOutcome, RetryPolicy, RunStatus, TaskRunRecord,
};
pub use session::{
    CircuitBreakerState, CircuitState, ClaudeSessionRecord, ContextPressureLevel,
    IterationAnalysis, ProgressSignal, SessionOutcome, SessionStatus, SessionTerminalClass,
    StopReason, TranscriptEvent,
};
pub use task::{BeadRef, BeadRuntimePatch, GroveBeadRecord, GroveBeadStatus};
pub use time::Timestamp;

pub const CRATE_PURPOSE: &str = "Shared Grove domain types and identifiers.";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::error::Error;
    use std::io::Error as IoError;

    type TestResult = Result<(), Box<dyn Error>>;

    #[test]
    fn bead_id_serde_roundtrip() -> TestResult {
        let id = BeadId::new("grove-123");
        let encoded = serde_json::to_string(&id)?;
        assert_eq!(encoded, "\"grove-123\"");
        let decoded: BeadId = serde_json::from_str(&encoded)?;
        assert_eq!(decoded, id);
        Ok(())
    }

    #[test]
    fn run_id_serde_roundtrip() -> TestResult {
        let id = RunId::new("run_123");
        let encoded = serde_json::to_string(&id)?;
        let decoded: RunId = serde_json::from_str(&encoded)?;
        assert_eq!(decoded, id);
        Ok(())
    }

    #[test]
    fn session_id_serde_roundtrip() -> TestResult {
        let id = SessionId::new("ses_123");
        let encoded = serde_json::to_string(&id)?;
        let decoded: SessionId = serde_json::from_str(&encoded)?;
        assert_eq!(decoded, id);
        Ok(())
    }

    #[test]
    fn checkpoint_id_serde_roundtrip() -> TestResult {
        let id = CheckpointId::new("chk_123");
        let encoded = serde_json::to_string(&id)?;
        let decoded: CheckpointId = serde_json::from_str(&encoded)?;
        assert_eq!(decoded, id);
        Ok(())
    }

    #[test]
    fn source_id_serde_roundtrip() -> TestResult {
        let id = SourceId::new("src_123");
        let encoded = serde_json::to_string(&id)?;
        let decoded: SourceId = serde_json::from_str(&encoded)?;
        assert_eq!(decoded, id);
        Ok(())
    }

    #[test]
    fn priority_ordering() {
        assert!(BeadPriority::P0 < BeadPriority::P1);
        assert!(BeadPriority::P3 < BeadPriority::P4);
    }

    #[test]
    fn priority_base_score_values() {
        assert_eq!(BeadPriority::P0.base_score(), 100);
        assert_eq!(BeadPriority::P1.base_score(), 70);
        assert_eq!(BeadPriority::P2.base_score(), 40);
        assert_eq!(BeadPriority::P3.base_score(), 20);
        assert_eq!(BeadPriority::P4.base_score(), 5);
    }

    #[test]
    fn grove_bead_status_all_variants_serialize() -> TestResult {
        for status in [
            GroveBeadStatus::Idle,
            GroveBeadStatus::Ready,
            GroveBeadStatus::Running,
            GroveBeadStatus::Checkpointed,
            GroveBeadStatus::WaitingToRetry,
            GroveBeadStatus::Succeeded,
            GroveBeadStatus::Failed,
        ] {
            let _encoded = serde_json::to_string(&status)?;
        }
        Ok(())
    }

    #[test]
    fn run_status_all_variants_serialize() -> TestResult {
        for status in [
            RunStatus::Active,
            RunStatus::WaitingToRetry,
            RunStatus::Checkpointed,
            RunStatus::Succeeded,
            RunStatus::Failed,
        ] {
            let _encoded = serde_json::to_string(&status)?;
        }
        Ok(())
    }

    #[test]
    fn recovery_capsule_outcomes_and_helpers() -> TestResult {
        for outcome in [
            RecoveryCapsuleOutcome::Failed,
            RecoveryCapsuleOutcome::Interrupted,
            RecoveryCapsuleOutcome::Checkpointed,
        ] {
            let _encoded = serde_json::to_string(&outcome)?;
            assert!(!outcome.as_str().is_empty());
        }

        let capsule = RecoveryCapsule::from_parts(
            RecoveryCapsuleOutcome::Checkpointed,
            None,
            None,
            Some("partial progress"),
            Some("resume verification"),
            Some("verification-first"),
            Some("narrowed prompt framing"),
            &[],
        )
        .ok_or_else(|| IoError::other("expected checkpoint capsule"))?;
        assert_eq!(capsule.recommended_next_step(), Some("resume verification"));
        Ok(())
    }

    #[test]
    fn agent_activity_all_variants_serialize() -> TestResult {
        for activity in [
            AgentActivity::Active,
            AgentActivity::Ready,
            AgentActivity::Idle,
            AgentActivity::Blocked,
            AgentActivity::Exited,
        ] {
            let _encoded = serde_json::to_string(&activity)?;
        }
        Ok(())
    }

    #[test]
    fn agent_activity_maps_to_autonomous_action() {
        assert!(matches!(
            AgentActivity::Active.autonomous_action(),
            AutonomousAction::Continue
        ));
        assert!(matches!(
            AgentActivity::Ready.autonomous_action(),
            AutonomousAction::CheckpointOrHandoff
        ));
        assert!(matches!(
            AgentActivity::Idle.autonomous_action(),
            AutonomousAction::InjectRescuePrompt
        ));
        assert!(matches!(
            AgentActivity::Blocked.autonomous_action(),
            AutonomousAction::RetryWithMutation
        ));
        assert!(matches!(
            AgentActivity::Exited.autonomous_action(),
            AutonomousAction::RecoverOrFail
        ));
    }

    #[test]
    fn escalation_tier_progression() {
        assert_eq!(EscalationTier::FirstAttempt.tier_number(), 0);
        assert_eq!(EscalationTier::SecondAttempt.tier_number(), 1);
        assert_eq!(EscalationTier::ThirdAttempt.tier_number(), 2);
        assert_eq!(EscalationTier::FinalAttempt.tier_number(), 3);
        assert_eq!(EscalationTier::GiveUp.tier_number(), 4);

        assert!(matches!(
            EscalationTier::FirstAttempt.escalate(),
            EscalationTier::SecondAttempt
        ));
        assert!(matches!(
            EscalationTier::SecondAttempt.escalate(),
            EscalationTier::ThirdAttempt
        ));
        assert!(matches!(
            EscalationTier::ThirdAttempt.escalate(),
            EscalationTier::FinalAttempt
        ));
        assert!(matches!(
            EscalationTier::FinalAttempt.escalate(),
            EscalationTier::GiveUp
        ));
        assert!(matches!(
            EscalationTier::GiveUp.escalate(),
            EscalationTier::GiveUp
        ));

        assert!(!EscalationTier::FirstAttempt.is_terminal());
        assert!(!EscalationTier::SecondAttempt.is_terminal());
        assert!(!EscalationTier::ThirdAttempt.is_terminal());
        assert!(!EscalationTier::FinalAttempt.is_terminal());
        assert!(EscalationTier::GiveUp.is_terminal());
    }

    #[test]
    fn escalation_tier_default() {
        assert!(matches!(
            EscalationTier::default(),
            EscalationTier::FirstAttempt
        ));
    }

    #[test]
    fn event_outcome_serializes() -> TestResult {
        for outcome in [
            EventOutcome::Success,
            EventOutcome::Failure,
            EventOutcome::Partial,
        ] {
            let _encoded = serde_json::to_string(&outcome)?;
        }
        Ok(())
    }

    #[test]
    fn event_kind_includes_recovery_capsule_created() -> TestResult {
        let encoded = serde_json::to_string(&EventKind::RecoveryCapsuleCreated)?;
        assert!(encoded.contains("RecoveryCapsuleCreated"));
        Ok(())
    }

    #[test]
    fn event_log_record_builder_pattern() -> TestResult {
        use chrono::Utc;
        let record = EventLogRecord::minimal(1, EventKind::RunStarted, Utc::now())
            .with_correlation("corr-123")
            .with_operation("spawn")
            .with_outcome(EventOutcome::Success)
            .with_duration(150)
            .with_error("timeout", "operation timed out", true);

        let json = serde_json::to_value(&record)?;
        assert_eq!(json["correlation_id"], "corr-123");
        assert_eq!(json["operation"], "spawn");
        assert_eq!(json["duration_ms"], 150);
        Ok(())
    }

    #[test]
    fn context_snapshot_serializes() -> TestResult {
        let snapshot = ContextSnapshot {
            context_usage_pct: Some(75.5),
            reservation_count: Some(3),
            escalation_tier: Some("SecondAttempt".into()),
            activity_state: Some("Active".into()),
        };
        let _encoded = serde_json::to_string(&snapshot)?;
        Ok(())
    }

    #[test]
    fn session_status_all_variants_serialize() -> TestResult {
        for status in [
            SessionStatus::Starting,
            SessionStatus::Running,
            SessionStatus::Checkpointed,
            SessionStatus::Completed,
            SessionStatus::TimedOut,
            SessionStatus::RateLimited,
            SessionStatus::PermissionDenied,
            SessionStatus::Crashed,
            SessionStatus::UnknownFailure,
        ] {
            let _encoded = serde_json::to_string(&status)?;
        }
        Ok(())
    }

    #[test]
    fn circuit_state_all_variants_serialize() -> TestResult {
        for state in [
            CircuitState::Closed,
            CircuitState::HalfOpen,
            CircuitState::Open,
        ] {
            let _encoded = serde_json::to_string(&state)?;
        }
        Ok(())
    }

    #[test]
    fn failure_class_all_variants_serialize() -> TestResult {
        for class in [
            FailureClass::Timeout,
            FailureClass::RateLimit,
            FailureClass::PermissionDenied,
            FailureClass::CircuitOpen,
            FailureClass::NoProgress,
            FailureClass::RepeatedError,
            FailureClass::ProtocolMalformed,
            FailureClass::ClaudeCrashed,
            FailureClass::BrMirrorFailed,
            FailureClass::Interrupted,
            FailureClass::Unknown,
        ] {
            let _encoded = serde_json::to_string(&class)?;
        }
        Ok(())
    }

    #[test]
    fn checkpoint_payload_with_all_fields() -> TestResult {
        let payload = CheckpointPayload {
            progress: "did x".into(),
            next_step: "do y".into(),
            context: json!({"foo": 1}),
            open_questions: vec!["q".into()],
            claimed_paths: vec!["src/**".into()],
            confidence: Some(0.7),
        };
        let value = serde_json::to_value(&payload)?;
        assert_eq!(value["progress"], "did x");
        assert_eq!(value["next_step"], "do y");
        let confidence = value["confidence"]
            .as_f64()
            .ok_or_else(|| IoError::other("missing confidence"))?;
        assert!((confidence - 0.7).abs() < 1e-6);
        Ok(())
    }

    #[test]
    fn checkpoint_payload_minimal() -> TestResult {
        let payload = CheckpointPayload {
            progress: String::new(),
            next_step: String::new(),
            context: Value::Null,
            open_questions: Vec::new(),
            claimed_paths: Vec::new(),
            confidence: None,
        };
        let _encoded = serde_json::to_string(&payload)?;
        Ok(())
    }

    #[test]
    fn protocol_event_result_serde() -> TestResult {
        let event = ProtocolEvent::Result {
            summary: "done".into(),
        };
        let encoded = serde_json::to_string(&event)?;
        assert!(encoded.contains("Result"));
        Ok(())
    }

    #[test]
    fn protocol_event_exit_true_serde() -> TestResult {
        let event = ProtocolEvent::Exit { value: true };
        let encoded = serde_json::to_string(&event)?;
        assert!(encoded.contains("true"));
        Ok(())
    }

    #[test]
    fn protocol_event_exit_false_serde() -> TestResult {
        let event = ProtocolEvent::Exit { value: false };
        let encoded = serde_json::to_string(&event)?;
        assert!(encoded.contains("false"));
        Ok(())
    }

    #[test]
    fn protocol_event_checkpoint_serde() -> TestResult {
        let event = ProtocolEvent::Checkpoint {
            payload: CheckpointPayload {
                progress: "halfway".into(),
                next_step: "finish".into(),
                context: json!({}),
                open_questions: Vec::new(),
                claimed_paths: Vec::new(),
                confidence: None,
            },
        };
        let encoded = serde_json::to_string(&event)?;
        assert!(encoded.contains("Checkpoint"));
        Ok(())
    }

    #[test]
    fn bullet_scope_all_variants() -> TestResult {
        for scope in [
            BulletScope::Global,
            BulletScope::Workspace,
            BulletScope::Language,
            BulletScope::Framework,
            BulletScope::Bead,
        ] {
            let _encoded = serde_json::to_string(&scope)?;
        }
        Ok(())
    }

    #[test]
    fn bullet_maturity_ordering() {
        assert!(BulletMaturity::Candidate < BulletMaturity::Established);
        assert!(BulletMaturity::Established < BulletMaturity::Proven);
        assert!(BulletMaturity::Proven < BulletMaturity::Deprecated);
    }

    #[test]
    fn feedback_kind_serde() -> TestResult {
        let helpful = serde_json::to_string(&FeedbackKind::Helpful)?;
        let harmful = serde_json::to_string(&FeedbackKind::Harmful)?;
        assert!(helpful.contains("Helpful"));
        assert!(harmful.contains("Harmful"));
        Ok(())
    }
}
