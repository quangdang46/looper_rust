#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use grove_types::{BeadId, RunId, Timestamp};
use std::{error::Error, fs, io::Error as IoError};
use tempfile::tempdir;

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn first_non_empty_line_prefers_stdout_content() {
    assert_eq!(
        first_non_empty_line("\n hello \nworld\n"),
        Some("hello".to_owned())
    );
}

#[test]
fn parse_version_line_extracts_semver() -> TestResult {
    let version =
        parse_version_line("br 0.1.12").ok_or_else(|| IoError::other("missing version"))?;
    assert_eq!(version.major, Some(0));
    assert_eq!(version.minor, Some(1));
    assert_eq!(version.patch, Some(12));
    Ok(())
}

#[test]
fn capability_reports_beads_dir() -> TestResult {
    let dir = tempdir()?;
    fs::create_dir(dir.path().join(".beads"))?;
    let client = CliBrClient::new("rustc", dir.path());
    let capability = client.capability()?;
    assert!(capability.available);
    assert!(capability.beads_dir_exists);
    assert!(capability.version_line.is_some());
    Ok(())
}

#[test]
fn missing_binary_returns_not_found() {
    let client = CliBrClient::new("definitely-not-a-real-br-binary", std::env::temp_dir());
    let err = client.capability().err();
    assert!(matches!(err, Some(BrError::NotFound { .. })));
}

#[test]
fn build_handoff_comment_combines_sections_into_one_comment() -> TestResult {
    let completed_at: Timestamp = "2026-03-20T05:00:00Z".parse()?;
    let handoff = HandoffRecord {
        bead_id: BeadId::new("grove-1j9.7.6"),
        run_id: RunId::new("run-123"),
        summary: "done".into(),
        artifacts: vec!["a.rs".into(), "b.rs".into()],
        lessons: vec!["lesson one".into()],
        decisions: vec!["decision one".into()],
        warnings: vec!["warning one".into()],
        completed_at,
    };

    let comment =
        build_handoff_comment(&handoff).ok_or_else(|| IoError::other("missing comment"))?;
    assert!(comment.contains("**Summary:** done"));
    assert!(comment.contains("**Artifacts:**\n- a.rs\n- b.rs"));
    assert!(comment.contains("**Lessons:**\n- lesson one"));
    assert!(comment.contains("**Decisions:**\n- decision one"));
    assert!(comment.contains("**Warnings:**\n- warning one"));
    Ok(())
}

#[test]
fn build_handoff_comment_returns_none_for_empty_handoff() {
    let handoff = HandoffRecord {
        bead_id: BeadId::new("grove-1j9.7.6"),
        run_id: RunId::new("run-123"),
        summary: String::new(),
        artifacts: Vec::new(),
        lessons: Vec::new(),
        decisions: Vec::new(),
        warnings: Vec::new(),
        completed_at: "2026-03-20T05:00:00Z".parse().unwrap(),
    };

    assert!(build_handoff_comment(&handoff).is_none());
}
