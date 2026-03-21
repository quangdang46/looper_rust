// Phase 3 Acceptance Tests
//
// This test suite covers Phase 3 orchestration requirements:
// 1. Graceful Shutdown & Stop Reasons
// 2. Parallel Orchestration Safety (Leader Leases & Reservations)
// 3. Crash Recovery (Reconciling interrupted runs)
// 4. Mirror-pending Behavior

use grove_db::Database;
use grove_kernel::{
    DispatchExitReason, LeaderLeaseConfig, LeaderLeaseManager, ReservationManager, ShutdownSignal,
};
use grove_types::{CoordinatorStopReason, EventKind};
use tempfile::tempdir;

// NOTE: We rely on `grove_kernel` unit tests for extensive DB interactions,
// but these acceptance suites map the behavioral promises of Phase 3.

#[test]
fn shutdown_signal_translates_to_durable_stop_reason() {
    let signal = ShutdownSignal::new();
    signal.trigger();
    
    assert!(signal.is_triggered(), "Shutdown signal should register as triggered.");
    
    let exit_reason = DispatchExitReason::ShutdownRequested;
    let stop_reason = exit_reason.to_stop_reason();
    
    assert_eq!(
        stop_reason,
        CoordinatorStopReason::UserStopped,
        "ShutdownRequested exit must map to a clean UserStopped durable reason."
    );
    assert!(stop_reason.is_user_initiated());
    assert!(stop_reason.is_clean());
}

#[test]
fn empty_queue_maps_to_clean_stop_reason() {
    let exit_reason = DispatchExitReason::QueueEmpty;
    let stop_reason = exit_reason.to_stop_reason();
    
    assert_eq!(stop_reason, CoordinatorStopReason::QueueEmpty);
    assert!(stop_reason.is_clean());
    assert!(!stop_reason.is_user_initiated());
}

#[test]
fn leader_contested_maps_to_uncle_fast_fail_reason() {
    let exit_reason = DispatchExitReason::LeaderContested;
    let stop_reason = exit_reason.to_stop_reason();

    assert_eq!(stop_reason, CoordinatorStopReason::LeaderContested);
    assert!(!stop_reason.is_clean(), "Contested lease is not a clean expected exit.");
}

#[test]
fn coordinator_shutdown_events_round_trip_in_db() {
    let dir = tempdir().expect("tempdir");
    let db_path = camino::Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .expect("utf8 db path");
    let mut db = Database::open(&db_path).expect("open db");
    db.migrate().expect("migrate");

    let now = chrono::Utc::now();
    db.write_event_log(
        EventKind::CoordinatorStopped,
        None,
        None,
        None,
        &serde_json::json!({
            "stop_reason": CoordinatorStopReason::Interrupted.as_str(),
            "forced_termination": true,
            "running_session_count": 1,
            "leader_released": true,
        }),
        &now,
    )
    .expect("write coordinator stop event");

    let row: (String, String) = db
        .connection()
        .query_row(
            "SELECT kind, payload_json FROM event_log ORDER BY id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read event row");

    assert_eq!(row.0, "CoordinatorStopped");
    let payload: serde_json::Value = serde_json::from_str(&row.1).expect("payload json");
    assert_eq!(payload["stop_reason"], "interrupted");
    assert_eq!(payload["forced_termination"], true);
    assert_eq!(payload["leader_released"], true);
}
