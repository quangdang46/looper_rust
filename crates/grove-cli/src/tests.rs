
use super::*;
use grove_kernel::{BlockedReasonCount, BlockedSampleBead, BlockedSampleReason};
use grove_types::{BeadRef, RunId, Timestamp};
use std::{error::Error, io};
use tempfile::tempdir;

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn live_transcript_tail_tolerates_partial_jsonl_writes() -> TestResult {
    let dir = tempdir()?;
    let path = dir.path().join("partial.jsonl");
    fs::write(
        &path,
        concat!(
            "{\"ts\":\"2026-03-23T00:00:00Z\",\"kind\":\"session_started\",\"session_id\":\"ses-1\"}\n",
            "{\"ts\":\"2026-03-23T00:00:01Z\",\"kind\":\"stdout\",\"line\":"
        ),
    )?;

    let transcript_path = path
        .to_str()
        .ok_or_else(|| io::Error::other("partial transcript path must be valid UTF-8"))?;
    let lines = read_live_transcript_lines(transcript_path)?;
    assert_eq!(
        lines,
        vec![
            "Transcript is still being written...".to_owned(),
            "Retrying on next refresh.".to_owned(),
        ]
    );
    Ok(())
}

#[test]
fn dispatch_blocked_summary_prints_retry_guidance() -> TestResult {
    let summary = DispatchBlockedSummary {
        blocked_ready_count: 1,
        reason_counts: vec![BlockedReasonCount {
            code: "failed_awaiting_manual_retry",
            summary: "failed and awaiting manual retry".to_owned(),
            count: 1,
        }],
        sample_beads: vec![BlockedSampleBead {
            bead_id: BeadId::new("saw-1rb"),
            reasons: vec![BlockedSampleReason {
                code: "failed_awaiting_manual_retry",
                summary: "failed and awaiting manual retry".to_owned(),
            }],
        }],
    };

    let payload = serde_json::to_value(summary)?;
    assert_eq!(payload["blocked_ready_count"], 1);
    assert_eq!(
        payload["reason_counts"][0]["code"],
        "failed_awaiting_manual_retry"
    );
    assert_eq!(payload["sample_beads"][0]["bead_id"], "saw-1rb");
    Ok(())
}

#[test]
fn should_autonomously_reset_only_crashed_manual_retry_bead() {
    let updated_at: Timestamp = chrono::Utc::now();
    let bead = GroveBeadRecord {
        bead: BeadRef {
            id: BeadId::new("saw-1rb"),
            title: "blocked root".to_owned(),
            description: None,
            priority: BeadPriority::P3,
            issue_type: "epic".to_owned(),
            br_status: "open".to_owned(),
            assignee: None,
            labels: vec!["release".to_owned()],
            created_at: updated_at,
            updated_at,
        },
        grove_status: GroveBeadStatus::Failed,
        declared_paths: vec![],
        metadata: serde_json::json!({}),
        last_run_id: Some(RunId::new("run-saw-1rb")),
        retry_after: None,
        last_failure_class: Some(grove_types::FailureClass::ClaudeCrashed),
        last_failure_detail: Some("session ended with Crash".to_owned()),
        circuit_breaker_state: None,
        synced_at: updated_at,
        runtime_updated_at: updated_at,
    };
    let blocked_reason_codes = vec!["failed_awaiting_manual_retry"];

    assert!(should_autonomously_reset_blocked_bead(
        &bead,
        &blocked_reason_codes
    ));

    let mut bead = bead;
    bead.last_failure_class = Some(grove_types::FailureClass::PermissionDenied);
    assert!(!should_autonomously_reset_blocked_bead(
        &bead,
        &blocked_reason_codes
    ));
}
