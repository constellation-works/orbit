use serde_json::Value;

use super::super::activity_v2::{ActivityV2, ActivityV2Spec, AgentLoopSpec, OnDenial, Provider};
use super::super::tool_allowlist::*;

#[test]
fn registry_validation_accepts_documented_empty_audit_root() {
    validate_tool_allowlist_against_registered_tools(
        &["orbit.audit.*".to_string()],
        ["orbit.task.show"],
    )
    .expect("reserved audit root is intentionally empty");
}

#[test]
fn registry_validation_rejects_unmatched_non_empty_root() {
    let err = validate_tool_allowlist_against_registered_tools(
        &["fs.*".to_string()],
        ["orbit.task.show"],
    )
    .expect_err("fs wildcard must match registered tools");

    assert_eq!(
        err,
        ToolAllowlistError::WildcardRootMatchesNoTools {
            entry: "fs.*".to_string()
        }
    );
}

#[test]
fn registry_validation_rejects_removed_graph_mcp_names() {
    let wildcard = validate_tool_allowlist(&["orbit.graph.*".to_string()])
        .expect_err("removed graph wildcard must fail");
    assert_eq!(
        wildcard,
        ToolAllowlistError::WildcardRootNotPermitted {
            entry: "orbit.graph.*".to_string()
        }
    );

    let concrete = validate_tool_allowlist_against_registered_tools(
        &["orbit.graph.search".to_string()],
        ["orbit.search", "orbit.task.show"],
    )
    .expect_err("removed graph tool name must fail");
    assert_eq!(
        concrete,
        ToolAllowlistError::UnknownToolName {
            entry: "orbit.graph.search".to_string()
        }
    );
}

#[test]
fn registry_validation_rejects_removed_session_log_mcp_names() {
    let wildcard = validate_tool_allowlist(&["orbit.session_log.*".to_string()])
        .expect_err("removed session-log wildcard must fail");
    assert_eq!(
        wildcard,
        ToolAllowlistError::WildcardRootNotPermitted {
            entry: "orbit.session_log.*".to_string()
        }
    );

    let concrete = validate_tool_allowlist_against_registered_tools(
        &["orbit.session_log.append".to_string()],
        ["orbit.search", "orbit.task.show"],
    )
    .expect_err("removed session-log tool name must fail");
    assert_eq!(
        concrete,
        ToolAllowlistError::UnknownToolName {
            entry: "orbit.session_log.append".to_string()
        }
    );
}

/// [ORB-10959] Granting `proc.spawn` without declaring the program allowlist
/// used to mean "unconstrained" — the omitted key was more permissive than an
/// explicit `[]`. Load-time validation now refuses the pairing.
#[test]
fn activity_validation_rejects_proc_spawn_grant_without_program_allowlist() {
    let activity = agent_loop_activity(vec!["proc.spawn".to_string()], None);

    let err = validate_activity_tool_allowlist(&activity)
        .expect_err("proc.spawn without an allowlist must fail closed");

    assert_eq!(
        err,
        ToolAllowlistError::ProcSpawnWithoutProgramAllowlist {
            entry: "proc.spawn".to_string()
        }
    );
    let message = err.to_string();
    assert!(message.contains("proc_allowed_programs: []"), "{message}");
}

/// A wildcard root that covers `proc.spawn` carries the same requirement, and
/// the error names the entry that granted it.
#[test]
fn activity_validation_rejects_proc_wildcard_without_program_allowlist() {
    let activity = agent_loop_activity(vec!["proc.*".to_string()], None);

    let err = validate_activity_tool_allowlist(&activity)
        .expect_err("proc.* without an allowlist must fail closed");

    assert_eq!(
        err,
        ToolAllowlistError::ProcSpawnWithoutProgramAllowlist {
            entry: "proc.*".to_string()
        }
    );
}

/// An explicit empty list is the documented way to deny every program, so it
/// must keep loading (enforcement stays with the `proc.spawn` tool gate).
#[test]
fn activity_validation_accepts_explicit_empty_program_allowlist() {
    let activity = agent_loop_activity(vec!["proc.spawn".to_string()], Some(Vec::new()));

    validate_activity_tool_allowlist(&activity)
        .expect("explicit deny-all allowlist is the supported opt-in");
}

/// A non-empty allowlist keeps permitting exactly the listed programs.
#[test]
fn activity_validation_accepts_declared_program_allowlist() {
    let activity = agent_loop_activity(
        vec!["proc.spawn".to_string()],
        Some(vec!["git".to_string(), "rg".to_string()]),
    );

    validate_activity_tool_allowlist(&activity).expect("declared allowlist loads");
    validate_activity_tool_allowlist_against_registered_tools(&activity, ["proc.spawn"])
        .expect("declared allowlist passes registry validation");
}

/// An activity that never grants `proc.spawn` may still omit the key.
#[test]
fn activity_validation_allows_missing_program_allowlist_without_proc_spawn() {
    let activity = agent_loop_activity(vec!["orbit.task.show".to_string()], None);

    validate_activity_tool_allowlist(&activity)
        .expect("an activity without proc.spawn needs no program allowlist");
}

/// The registry-aware entry point enforces the same pairing, so an activity
/// reaching the catalog cannot skip the check the asset loader applies.
#[test]
fn registry_validation_rejects_proc_spawn_grant_without_program_allowlist() {
    let activity = agent_loop_activity(vec!["proc.spawn".to_string()], None);

    let err = validate_activity_tool_allowlist_against_registered_tools(&activity, ["proc.spawn"])
        .expect_err("catalog validation must fail closed too");

    assert_eq!(
        err,
        ToolAllowlistError::ProcSpawnWithoutProgramAllowlist {
            entry: "proc.spawn".to_string()
        }
    );
}

fn agent_loop_activity(
    tools: Vec<String>,
    proc_allowed_programs: Option<Vec<String>>,
) -> ActivityV2 {
    ActivityV2 {
        description: "test".to_string(),
        input_schema_json: Value::Null,
        output_schema_json: Value::Null,
        fs_profile: None,
        spec: ActivityV2Spec::AgentLoop(AgentLoopSpec {
            instruction: "test".to_string(),
            tools,
            on_denial: OnDenial::Terminate,
            model: None,
            reasoning_effort: None,
            max_iterations: 1,
            backend: None,
            provider: Provider::default(),
            wall_clock_timeout_seconds: 30,
            require_response_envelope: false,
            require_completion_envelope: true,
            proc_allowed_programs,
            trusted_host_execution: false,
        }),
    }
}
