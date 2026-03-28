use super::*;
use std::{error::Error, io::Error as IoError};

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn parse_ready_envelope_shape() -> TestResult {
    let parsed = parse_ready_output(
        r#"{"issues":[{"id":"bd-1","title":"A","priority":1,"issue_type":"task","status":"open","assignee":null,"labels":[],"created_at":"2026-03-10T08:00:00Z","updated_at":"2026-03-11T09:00:00Z","blocked_by":[],"blocks":[]}],"count":1}"#,
    )?;
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].id.as_str(), "bd-1");
    assert_eq!(parsed[0].priority, BeadPriority::P1);
    Ok(())
}

#[test]
fn parse_ready_live_array_shape() -> TestResult {
    let parsed = parse_ready_output(
        r#"[{"id":"grove-1","title":"A","description":"Desc","priority":0,"issue_type":"task","status":"in_progress","created_at":"2026-03-15T13:42:52.385028328Z","updated_at":"2026-03-16T01:09:52.383821233Z","labels":["area:db"]}]"#,
    )?;
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].status, "in_progress");
    assert_eq!(parsed[0].blocked_by, Vec::<BeadId>::new());
    Ok(())
}

#[test]
fn parse_show_singleton_array_shape() -> TestResult {
    let bead_id = BeadId::new("grove-1j9.5.5");
    let parsed = parse_show_output(
        r#"[{"id":"grove-1j9.5.5","title":"Implement grove-br","description":"Desc","priority":0,"issue_type":"task","status":"open","created_at":"2026-03-15T13:42:52.425792656Z","updated_at":"2026-03-16T01:50:27.108888393Z","labels":["area:br"],"dependencies":[{"id":"grove-1j9.5.4","dependency_type":"blocks"}],"dependents":[{"id":"grove-1j9.5.10","dependency_type":"blocks"}],"comments":[{"id":16,"text":"hello","author":"RedIsland","created_at":"2026-03-15T13:43:39Z"}]}]"#,
        &bead_id,
    )?;
    assert_eq!(parsed.summary.id, bead_id);
    assert_eq!(parsed.summary.blocked_by[0].as_str(), "grove-1j9.5.4");
    assert_eq!(parsed.summary.blocks[0].as_str(), "grove-1j9.5.10");
    assert_eq!(parsed.comments[0].id, "16");
    Ok(())
}

#[test]
fn parse_list_live_array_shape() -> TestResult {
    let parsed = parse_list_output(
        r#"[{"id":"grove-1j9.5.5","title":"Implement grove-br","description":"Desc","priority":0,"issue_type":"task","status":"in_progress","created_at":"2026-03-15T13:42:52.425792656Z","updated_at":"2026-03-16T02:06:37.842705419Z","labels":["area:br"]}]"#,
    )?;
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].id.as_str(), "grove-1j9.5.5");
    assert_eq!(parsed[0].status, "in_progress");
    Ok(())
}

#[test]
fn parse_show_empty_array_is_not_found() -> TestResult {
    let bead_id = BeadId::new("missing");
    let err = parse_show_output("[]", &bead_id)
        .err()
        .ok_or_else(|| IoError::other("expected error"))?;
    assert!(matches!(err, ShowParseError::NotFound(id) if id == bead_id));
    Ok(())
}

#[test]
fn parse_dep_rows_live_shape() -> TestResult {
    let bead_id = BeadId::new("grove-1j9.5.5");
    let parsed = parse_dep_list_output(
        r#"[{"issue_id":"grove-1j9.5.5","depends_on_id":"grove-1j9.5.4","type":"blocks","title":"DB","status":"in_progress","priority":0}]"#,
        &bead_id,
    )?;
    assert_eq!(parsed.bead_id, bead_id);
    assert_eq!(parsed.blocked_by.len(), 1);
    assert_eq!(parsed.blocked_by[0].as_str(), "grove-1j9.5.4");
    assert_eq!(parsed.rows.len(), 1);
    Ok(())
}

#[test]
fn version_parse_extracts_semver_components() {
    let version = super::super::client::parse_version_line("br 0.1.12");
    assert_eq!(version.as_ref().and_then(|v| v.major), Some(0));
    assert_eq!(version.as_ref().and_then(|v| v.minor), Some(1));
    assert_eq!(version.as_ref().and_then(|v| v.patch), Some(12));
}
