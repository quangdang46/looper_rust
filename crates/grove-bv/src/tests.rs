use super::*;
use std::{error::Error, io::Error as IoError};

#[cfg(unix)]
use std::{fs, os::unix::fs::PermissionsExt};
#[cfg(unix)]
use tempfile::tempdir;

type TestResult = Result<(), Box<dyn Error>>;

#[cfg(unix)]
fn write_mock_bv_script(path: &std::path::Path) -> TestResult {
    let temp_path = path.with_extension("tmp");
    fs::write(&temp_path, "#!/bin/sh\nprintf 'bv 1.2.3\\n'\n")?;

    let mut permissions = fs::metadata(&temp_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&temp_path, permissions)?;
    fs::rename(&temp_path, path)?;
    Ok(())
}

#[test]
fn parse_triage_live_shape() -> TestResult {
    let parsed = parse_triage_output(
        r#"{
                "generated_at":"2026-03-16T03:27:38Z",
                "data_hash":"abc123",
                "triage":{
                    "meta":{
                        "version":"1.0.0",
                        "generated_at":"2026-03-16T10:27:38+07:00",
                        "phase2_ready":true,
                        "issue_count":58,
                        "compute_time_ms":0
                    },
                    "quick_ref":{
                        "open_count":53,
                        "actionable_count":2,
                        "blocked_count":51,
                        "in_progress_count":2,
                        "top_picks":[{
                            "id":"grove-1j9.5.6",
                            "title":"Implement grove-bv",
                            "score":0.42,
                            "reasons":["available"],
                            "unblocks":1
                        }]
                    },
                    "recommendations":[{
                        "id":"grove-1j9.5.8",
                        "title":"Implement CLI commands",
                        "type":"task",
                        "status":"open",
                        "priority":0,
                        "labels":["area:cli"],
                        "score":0.38,
                        "breakdown":{"pagerank":0.15,"betweenness":0.16},
                        "action":"Work on grove-1j9.5.5 first",
                        "reasons":["blocked"],
                        "unblocks_ids":["grove-1j9.5.9"],
                        "blocked_by":["grove-1j9.5.5","grove-1j9.5.6"]
                    }],
                    "quick_wins":[{
                        "id":"grove-1j9.9.6",
                        "title":"Acceptance coverage",
                        "score":0.36,
                        "reason":"high leverage",
                        "unblocks_ids":["grove-1j9.10.1"]
                    }],
                    "blockers_to_clear":[{
                        "id":"grove-1j9.5.5",
                        "title":"Implement grove-br",
                        "unblocks_count":1,
                        "unblocks_ids":["grove-1j9.5.10"],
                        "actionable":true,
                        "blocked_by":[]
                    }],
                    "project_health":{
                        "counts":{
                            "total":58,
                            "open":53,
                            "closed":5,
                            "blocked":51,
                            "actionable":2,
                            "by_status":{"open":53},
                            "by_type":{"task":50},
                            "by_priority":{"P0":20}
                        },
                        "graph":{
                            "node_count":58,
                            "edge_count":107,
                            "density":0.03,
                            "has_cycles":false,
                            "phase2_ready":true
                        },
                        "velocity":{
                            "closed_last_7_days":5,
                            "closed_last_30_days":5,
                            "avg_days_to_close":0.3,
                            "weekly":[
                                {"week_start":"2026-03-10T00:00:00Z","closed":2},
                                {"week_start":"2026-03-03T00:00:00Z","closed":3}
                            ]
                        }
                    },
                    "commands":{
                        "claim_top":"bd update grove-1j9.5.6 --status=in_progress",
                        "show_top":"bd show grove-1j9.5.6 --json"
                    }
                },
                "usage_hints":["hint"]
            }"#,
    )?;

    assert_eq!(parsed.quick_ref.top_picks[0].id.as_str(), "grove-1j9.5.6");
    assert_eq!(parsed.recommendations[0].priority, BeadPriority::P0);
    assert_eq!(
        parsed.recommendations[0].unblocks[0].as_str(),
        "grove-1j9.5.9"
    );
    assert_eq!(parsed.commands.len(), 2);
    assert_eq!(parsed.commands[0].label, "claim_top");
    assert_eq!(parsed.project_health.velocity.weekly, vec![2, 3]);
    Ok(())
}

#[test]
fn parse_triage_accepts_numeric_weekly_velocity() -> TestResult {
    let parsed = parse_triage_output(
        r#"{
                "generated_at":"2026-03-16T03:27:38Z",
                "data_hash":"abc123",
                "triage":{
                    "meta":{
                        "version":"1.0.0",
                        "generated_at":"2026-03-16T10:27:38+07:00",
                        "phase2_ready":true,
                        "issue_count":58,
                        "compute_time_ms":0
                    },
                    "quick_ref":{
                        "open_count":53,
                        "actionable_count":2,
                        "blocked_count":51,
                        "in_progress_count":2,
                        "top_picks":[]
                    },
                    "recommendations":[],
                    "quick_wins":[],
                    "blockers_to_clear":[],
                    "project_health":{
                        "counts":{
                            "total":58,
                            "open":53,
                            "closed":5,
                            "blocked":51,
                            "actionable":2,
                            "by_status":{"open":53},
                            "by_type":{"task":50},
                            "by_priority":{"0":20}
                        },
                        "graph":{
                            "node_count":58,
                            "edge_count":107,
                            "density":0.03,
                            "has_cycles":false,
                            "phase2_ready":true
                        },
                        "velocity":{
                            "closed_last_7_days":5,
                            "closed_last_30_days":5,
                            "avg_days_to_close":0.3,
                            "weekly":[2,3]
                        }
                    },
                    "commands":{}
                },
                "usage_hints":[]
            }"#,
    )?;

    assert_eq!(parsed.project_health.velocity.weekly, vec![2, 3]);
    Ok(())
}

#[test]
fn parse_next_live_shape() -> TestResult {
    let parsed = parse_next_output(
        r#"{
                "generated_at":"2026-03-16T03:29:12Z",
                "data_hash":"abc123",
                "id":"grove-1j9.5.6",
                "title":"Implement grove-bv",
                "score":0.34,
                "reasons":["available"],
                "unblocks":1,
                "claim_command":"bd update grove-1j9.5.6 --status=in_progress",
                "show_command":"bd show grove-1j9.5.6"
            }"#,
    )?;

    assert_eq!(parsed.id.as_str(), "grove-1j9.5.6");
    assert_eq!(parsed.unblocks, 1);
    assert!(parsed.claim_command.is_some());
    Ok(())
}

#[test]
fn parse_plan_live_shape() -> TestResult {
    let parsed = parse_plan_output(
        r#"{
                "generated_at":"2026-03-16T03:29:12Z",
                "data_hash":"abc123",
                "analysis_config":{"ComputeSlack":true},
                "status":{
                    "PageRank":{"state":"skipped"},
                    "Slack":{"state":"computed"}
                },
                "plan":{
                    "tracks":[{
                        "track_id":"track-A",
                        "items":[{
                            "id":"grove-1j9.5.6",
                            "title":"Implement grove-bv",
                            "priority":1,
                            "status":"in_progress",
                            "unblocks":["grove-1j9.5.8"]
                        }],
                        "reason":"Independent work stream"
                    }],
                    "total_actionable":2,
                    "total_blocked":51,
                    "summary":{
                        "highest_impact":"grove-1j9.5.6",
                        "impact_reason":"Unblocks 1 task",
                        "unblocks_count":1
                    }
                },
                "usage_hints":["hint"]
            }"#,
    )?;

    assert_eq!(parsed.tracks[0].items[0].priority, BeadPriority::P1);
    assert_eq!(
        parsed.summary.highest_impact.as_ref().map(BeadId::as_str),
        Some("grove-1j9.5.6")
    );
    assert_eq!(parsed.status["Slack"].state, "computed");
    Ok(())
}

#[test]
fn parse_insights_live_shape() -> TestResult {
    let parsed = parse_insights_output(
        r#"{
                "generated_at":"2026-03-16T03:29:13Z",
                "data_hash":"abc123",
                "analysis_config":{"ComputePageRank":true},
                "status":{"PageRank":{"state":"computed"}},
                "Bottlenecks":[{"ID":"grove-1j9.5.8","Value":198.2}],
                "Keystones":[{"ID":"grove-1j9.5.6","Value":35}],
                "Influencers":[{"ID":"grove-1j9.5.6","Value":0}],
                "Hubs":[{"ID":"grove-1j9.2","Value":0.66}],
                "Authorities":[{"ID":"grove-1j9.8.6","Value":0.77}],
                "Cores":[{"ID":"grove-1j9","Value":3}],
                "Articulation":null,
                "Slack":[{"ID":"grove-1j9.1","Value":39}],
                "Orphans":["grove-1j9.1"],
                "Cycles":[["grove-a","grove-b"]],
                "ClusterDensity":0.036,
                "Velocity":{
                    "closed_last_7_days":5,
                    "closed_last_30_days":5,
                    "avg_days_to_close":0.3,
                    "weekly":[2,3]
                },
                "Stats":{
                    "OutDegree":{"grove-1j9.5.6":2},
                    "InDegree":{"grove-1j9.5.8":1},
                    "TopologicalOrder":["grove-1j9.5.6","grove-1j9.5.8"],
                    "Density":0.036,
                    "NodeCount":58,
                    "EdgeCount":107,
                    "Config":{"mode":"exact"}
                },
                "full_stats":{
                    "pagerank":{"grove-1j9.5.6":0.0198},
                    "betweenness":{"grove-1j9.5.6":0.02},
                    "eigenvector":{"grove-1j9.5.6":0.01},
                    "hubs":{"grove-1j9.5.6":0.02},
                    "authorities":{"grove-1j9.5.8":0.04},
                    "critical_path_score":{"grove-1j9.5.6":35},
                    "core_number":{"grove-1j9.5.6":3},
                    "slack":{"grove-1j9.5.6":2},
                    "articulation_points":["grove-1j9.5.6"]
                },
                "top_what_ifs":[{
                    "issue_id":"grove-1j9.5.8",
                    "title":"Implement CLI commands",
                    "delta":{
                        "direct_unblocks":1,
                        "transitive_unblocks":19,
                        "blocked_reduction":0,
                        "depth_reduction":1,
                        "estimated_days_saved":0.125,
                        "unblocked_issue_ids":["grove-1j9.5.9"],
                        "parallelization_gain":0,
                        "explanation":"Completing this directly unblocks 1 item"
                    }
                }],
                "advanced_insights":{"usage_hints":["hint"]}
            }"#,
    )?;

    assert_eq!(parsed.bottlenecks[0].id.as_str(), "grove-1j9.5.8");
    assert_eq!(parsed.articulation_points[0].as_str(), "grove-1j9.5.6");
    assert_eq!(parsed.cycles[0][0].as_str(), "grove-a");
    assert_eq!(
        parsed.full_stats.page_rank[&BeadId::new("grove-1j9.5.6")],
        0.0198
    );
    Ok(())
}

#[test]
fn parse_priority_audit_live_shape() -> TestResult {
    let parsed = parse_priority_audit_output(
        r#"{
                "generated_at":"2026-03-16T03:27:10Z",
                "data_hash":"abc123",
                "analysis_config":{},
                "field_descriptions":{"confidence":"why"},
                "filters":{"label":"phase:1"},
                "recommendations":[{
                    "issue_id":"grove-1j9.5.6",
                    "title":"Implement grove-bv",
                    "current_priority":1,
                    "suggested_priority":0,
                    "direction":"raise",
                    "confidence":0.95,
                    "impact_score":0.8,
                    "explanation":"high leverage",
                    "reasoning":["critical path"],
                    "what_if":{"delta":1}
                }],
                "status":{"PageRank":{"state":"computed"}},
                "summary":{"total":1},
                "usage_hints":["hint"]
            }"#,
    )?;

    assert_eq!(parsed.recommendations[0].current_priority, BeadPriority::P1);
    assert_eq!(
        parsed.recommendations[0].suggested_priority,
        BeadPriority::P0
    );
    assert_eq!(parsed.status["PageRank"].state, "computed");
    Ok(())
}

#[test]
fn parse_alerts_live_shape() -> TestResult {
    let parsed = parse_alerts_output(
        r#"{
                "generated_at":"2026-03-16T03:27:10Z",
                "data_hash":"abc123",
                "output_format":"json",
                "alerts":[{"issue_id":"grove-1j9.5.6","severity":"warning"}],
                "summary":{"total":1,"critical":0,"warning":1,"info":0},
                "usage_hints":["hint"]
            }"#,
    )?;

    assert_eq!(parsed.summary.warning, 1);
    assert_eq!(parsed.alerts.len(), 1);
    Ok(())
}

#[test]
fn first_non_empty_line_prefers_stdout_content() {
    assert_eq!(
        first_non_empty_line("\n hello \nworld\n"),
        Some("hello".to_owned())
    );
}

#[cfg(unix)]
#[test]
fn capability_reports_beads_dir_from_mock_binary() -> TestResult {
    let dir = tempdir()?;
    fs::create_dir(dir.path().join(".beads"))?;
    let script = dir.path().join("mock-bv");
    write_mock_bv_script(&script)?;

    let client = CliBvClient::new(script.display().to_string(), dir.path());
    let capability = client.capability()?;
    assert!(capability.available);
    assert!(capability.beads_dir_exists);
    assert_eq!(capability.version.as_deref(), Some("bv 1.2.3"));
    Ok(())
}

#[test]
fn missing_binary_returns_not_found() {
    let client = CliBvClient::new("definitely-not-a-real-bv-binary", std::env::temp_dir());
    let err = client.capability().err();
    assert!(matches!(err, Some(BvError::NotFound { .. })));
}

#[test]
fn unsupported_priority_returns_parse_error() -> TestResult {
    let err = parse_next_output(
        r#"{
                "generated_at":"2026-03-16T03:29:12Z",
                "data_hash":"abc123",
                "id":"grove-1j9.5.6",
                "title":"Implement grove-bv",
                "score":0.34,
                "reasons":[],
                "unblocks":1
            }"#,
    )?;
    assert_eq!(err.id.as_str(), "grove-1j9.5.6");
    Ok(())
}

#[test]
fn invalid_priority_recommendation_priority_is_rejected() -> TestResult {
    let err = parse_priority_audit_output(
        r#"{
                "generated_at":"2026-03-16T03:27:10Z",
                "data_hash":"abc123",
                "analysis_config":{},
                "field_descriptions":{},
                "filters":{},
                "recommendations":[{
                    "issue_id":"grove-1j9.5.6",
                    "title":"Implement grove-bv",
                    "current_priority":9,
                    "suggested_priority":0,
                    "direction":"raise",
                    "confidence":0.95,
                    "impact_score":0.8,
                    "explanation":"high leverage"
                }],
                "status":{},
                "summary":{},
                "usage_hints":[]
            }"#,
    )
    .err()
    .ok_or_else(|| IoError::other("expected parse error"))?;

    assert!(
        err.to_string().contains("unsupported bead priority`9`")
            || err.to_string().contains("unsupported bead priority `9`")
    );
    Ok(())
}
