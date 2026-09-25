use super::super::*;
use orbit_common::test_fixtures::{TEST_CLAUDE_MODEL, TEST_CODEX_MODEL, TEST_GROK_MODEL};

use crate::{AuditToolCallCountsByRole, AuditToolCallCountsBySurfaceAndRole};
use std::fs;

#[test]
fn summary_overlays_audit_tool_call_counts_by_normalized_model() {
    let temp = tempfile::tempdir().expect("create tempdir");

    let summary = generate_summary_with_audit_tool_calls(
        temp.path(),
        &[],
        &[
            AuditToolCallCountsByRole {
                role: "codex / gpt-5".to_string(),
                total: 2,
                failed: 1,
            },
            AuditToolCallCountsByRole {
                role: "gpt-5".to_string(),
                total: 1,
                failed: 1,
            },
        ],
    )
    .expect("generate summary");

    let codex = summary.agents.get("codex").expect("codex summary");
    assert_eq!(codex.tool_calls, 3);
    assert_eq!(codex.failed_tool_calls, 2);
}

#[test]
fn summary_includes_zero_rows_for_known_families() {
    let temp = tempfile::tempdir().expect("create tempdir");

    let summary = generate_summary(temp.path(), &[]).expect("generate summary");

    let grok = summary.agents.get("grok").expect("grok summary");
    assert_eq!(grok.tasks_completed, 0);
}

#[test]
fn summary_agent_keys_are_families_not_models() {
    let temp = tempfile::tempdir().expect("create tempdir");
    fs::create_dir_all(temp.path()).expect("create scoreboard dir");
    fs::write(
        temp.path().join("tokens.json"),
        r#"{
              "agents": [
                { "agent": "codex", "model": "gpt-5.5", "total_tokens": 1 },
                { "agent": "claude", "model": "claude-opus-4-7", "total_tokens": 1 },
                { "agent": "grok", "model": "grok-4", "total_tokens": 1 }
              ]
            }"#,
    )
    .expect("write tokens scoreboard");

    let summary = generate_summary(temp.path(), &[]).expect("generate summary");
    for forbidden in [TEST_GROK_MODEL, TEST_CLAUDE_MODEL, TEST_CODEX_MODEL] {
        assert!(
            !summary.agents.contains_key(forbidden),
            "model key leaked into summary agents: {forbidden}"
        );
    }
    for family in ["codex", "claude", "gemini", "grok"] {
        assert!(summary.agents.contains_key(family));
    }
}

#[test]
fn audit_tool_calls_do_not_double_count_token_scoreboard_tool_calls() {
    let temp = tempfile::tempdir().expect("create tempdir");
    fs::create_dir_all(temp.path()).expect("create scoreboard dir");
    fs::write(
        temp.path().join("tokens.json"),
        r#"{
              "agents": [
                {
                  "agent": "codex",
                  "model": "gpt-5",
                  "total_tokens": 10,
                  "total_output_tokens": 4,
                  "total_tool_calls": 5
                }
              ]
            }"#,
    )
    .expect("write tokens scoreboard");

    let summary = generate_summary_with_audit_tool_calls(
        temp.path(),
        &[],
        &[AuditToolCallCountsByRole {
            role: "gpt-5".to_string(),
            total: 3,
            failed: 2,
        }],
    )
    .expect("generate summary");

    let codex = summary.agents.get("codex").expect("codex summary");
    assert_eq!(codex.tokens.total, 10);
    assert_eq!(codex.tokens.output, 4);
    assert_eq!(codex.tool_calls, 5);
    assert_eq!(codex.failed_tool_calls, 2);
}

#[test]
fn audit_tool_calls_win_when_larger_than_token_scoreboard_tool_calls() {
    let temp = tempfile::tempdir().expect("create tempdir");
    fs::create_dir_all(temp.path()).expect("create scoreboard dir");
    fs::write(
        temp.path().join("tokens.json"),
        r#"{
              "agents": [
                {
                  "agent": "codex",
                  "model": "gpt-5",
                  "total_tokens": 10,
                  "total_output_tokens": 4,
                  "total_tool_calls": 2
                }
              ]
            }"#,
    )
    .expect("write tokens scoreboard");

    let summary = generate_summary_with_audit_tool_calls(
        temp.path(),
        &[],
        &[AuditToolCallCountsByRole {
            role: "gpt-5".to_string(),
            total: 7,
            failed: 3,
        }],
    )
    .expect("generate summary");

    let codex = summary.agents.get("codex").expect("codex summary");
    assert_eq!(codex.tokens.total, 10);
    assert_eq!(codex.tokens.output, 4);
    assert_eq!(codex.tool_calls, 7);
    assert_eq!(codex.failed_tool_calls, 3);
}

#[test]
fn summary_overlays_per_surface_tool_call_counts() {
    let temp = tempfile::tempdir().expect("create tempdir");

    let surface_rows = vec![
        AuditToolCallCountsBySurfaceAndRole {
            surface: "graph".to_string(),
            role: TEST_CLAUDE_MODEL.to_string(),
            total: 56,
            failed: 2,
        },
        AuditToolCallCountsBySurfaceAndRole {
            surface: "graph".to_string(),
            role: TEST_CODEX_MODEL.to_string(),
            total: 697,
            failed: 5,
        },
        AuditToolCallCountsBySurfaceAndRole {
            surface: "task".to_string(),
            role: TEST_CODEX_MODEL.to_string(),
            total: 410,
            failed: 1,
        },
    ];

    let summary = generate_summary_with_inputs(
        temp.path(),
        &[],
        &ScoreboardInputs {
            audit_tool_calls_by_surface: &surface_rows,
            ..ScoreboardInputs::default()
        },
    )
    .expect("generate summary");

    let claude = summary.agents.get("claude").expect("claude summary");
    assert_eq!(claude.tool_calls_by_surface.get("graph").copied(), Some(56));
    assert_eq!(claude.tool_calls_by_surface.get("task"), None);

    let codex = summary.agents.get("codex").expect("codex summary");
    assert_eq!(codex.tool_calls_by_surface.get("graph").copied(), Some(697));
    assert_eq!(codex.tool_calls_by_surface.get("task").copied(), Some(410));
}

#[test]
fn summary_exposes_friction_reported_counts_from_records() {
    // Deterministic test per ORB-00143: seeds friction records for >=2 families
    // and asserts the generated scoreboard exposes nonzero `friction.reported`
    // (and zero for families with none). Uses the inputs path so it does not
    // depend on disk state.
    let temp = tempfile::tempdir().expect("create tempdir");

    let friction_reported = vec![
        crate::friction_store::FrictionReportedCount {
            model: "codex".to_string(),
            count: 1,
        },
        crate::friction_store::FrictionReportedCount {
            model: "claude-3-opus".to_string(),
            count: 1,
        },
    ];

    let summary = generate_summary_with_inputs(
        temp.path(),
        &[],
        &ScoreboardInputs {
            friction_reported: &friction_reported,
            ..ScoreboardInputs::default()
        },
    )
    .expect("generate summary with seeded frictions");

    let codex = summary.agents.get("codex").expect("codex summary");
    assert_eq!(
        codex.friction.reported, 1,
        "codex should report 1 friction record"
    );

    let claude = summary.agents.get("claude").expect("claude summary");
    assert_eq!(
        claude.friction.reported, 1,
        "claude (from claude-3-opus) should report 1"
    );

    let gemini = summary.agents.get("gemini").expect("gemini summary");
    assert_eq!(
        gemini.friction.reported, 0,
        "gemini with no records must expose 0, not fall back"
    );

    let grok = summary.agents.get("grok").expect("grok summary");
    assert_eq!(grok.friction.reported, 0);
}
