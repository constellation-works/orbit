use serde_json::Value;

use super::super::activity_v2::{ActivityV2, ActivityV2Spec, AgentLoopSpec, OnDenial, Provider};
use super::super::tool_allowlist::*;

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
            proc_disallowed_programs: None,
            trusted_host_execution: false,
            tool_disallow_list: None,
        }),
    }
}
