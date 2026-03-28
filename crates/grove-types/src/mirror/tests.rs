    use super::*;
    use serde_json::json;

    #[test]
    fn mirror_operation_retry_logic() {
        let timestamp = "2026-03-19T10:00:00Z".parse().unwrap();
        let mut op = MirrorOperation::new(
            1,
            BeadId::new("grove-1"),
            RunId::new("run-1"),
            MirrorOperationType::Close,
            json!({}).to_string(),
            timestamp,
        );

        // Initially can retry
        assert!(op.can_retry(3));
        assert!(!op.is_terminal());

        // After first attempt, still pending
        op.mark_failure("temp error".into(), timestamp, 3);
        assert!(op.can_retry(3));
        assert!(!op.is_terminal());

        // After max attempts, should be failed
        op.mark_failure("final error".into(), timestamp, 1);
        assert!(!op.can_retry(1));
        assert!(op.is_terminal());
    }

    #[test]
    fn mirror_operation_success_flow() {
        let timestamp = "2026-03-19T10:00:00Z".parse().unwrap();
        let mut op = MirrorOperation::new(
            1,
            BeadId::new("grove-1"),
            RunId::new("run-1"),
            MirrorOperationType::Close,
            json!({}).to_string(),
            timestamp,
        );

        op.mark_in_flight(timestamp);
        assert_eq!(op.state, MirrorState::InFlight);
        assert_eq!(op.attempt_count, 1);

        op.mark_success(timestamp);
        assert_eq!(op.state, MirrorState::Succeeded);
        assert!(op.is_terminal());
        assert!(op.last_error.is_none());
    }

    #[test]
    fn mirror_state_display_names() {
        assert_eq!(MirrorState::Pending.display_name(), "pending");
        assert_eq!(MirrorState::InFlight.display_name(), "in-flight");
        assert_eq!(MirrorState::Succeeded.display_name(), "succeeded");
        assert_eq!(MirrorState::Failed.display_name(), "failed");
    }

    #[test]
    fn mirror_batch_result_tracking() {
        let mut result = MirrorBatchResult::default();

        result.record_success();
        result.record_success();
        result.record_failure(MirrorError {
            bead_id: BeadId::new("grove-1"),
            operation_id: 1,
            operation_type: MirrorOperationType::Close,
            error_message: "failed".into(),
        });
        result.record_pending();

        assert_eq!(result.succeeded, 2);
        assert_eq!(result.failed, 1);
        assert_eq!(result.still_pending, 1);
        assert_eq!(result.total_attempted(), 3);
        assert!(result.has_failures());
        assert!(!result.is_complete());
    }

    #[test]
    fn mirror_operation_type_serialization() {
        let types = [
            MirrorOperationType::Close,
            MirrorOperationType::UpdateStatus,
            MirrorOperationType::AddComment,
            MirrorOperationType::Sync,
        ];

        for op_type in types {
            let serialized = serde_json::to_string(&op_type).unwrap();
            let deserialized: MirrorOperationType = serde_json::from_str(&serialized).unwrap();
            assert_eq!(op_type, deserialized);
        }
    }

    #[test]
    fn close_payload_serialization() {
        let payload = ClosePayload {
            reason: Some("done".into()),
            comment: Some("finished task".into()),
        };

        let serialized = serde_json::to_string(&payload).unwrap();
        let deserialized: ClosePayload = serde_json::from_str(&serialized).unwrap();

        assert_eq!(deserialized.reason, payload.reason);
        assert_eq!(deserialized.comment, payload.comment);
    }
