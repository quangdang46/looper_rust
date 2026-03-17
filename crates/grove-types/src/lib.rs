mod archive;
mod checkpoint;
mod errors;
mod event;
mod handoff;
mod ids;
mod playbook;
mod priority;
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
pub use event::{EventKind, EventLogRecord};
pub use handoff::HandoffRecord;
pub use ids::{BeadId, BulletId, CheckpointId, PromptId, RunId, SessionId, SourceId, TickId};
pub use playbook::{
    BulletMaturity, BulletScope, BulletState, BulletType, FeedbackEventRecord, FeedbackKind,
    MemoryDiaryRecord, PlaybookBulletRecord,
};
pub use priority::BeadPriority;
pub use reservation::{ReservationConflict, ReservationMode, ReservationRecord};
pub use run::{FailureClass, RetryPolicy, RunStatus, TaskRunRecord};
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
