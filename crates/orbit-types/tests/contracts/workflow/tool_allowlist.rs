use serde_json::Value;

use orbit_types::workflow::activity_job::{ActivityV2, ActivityV2Spec, AgentLoopSpec, Provider};
use orbit_types::workflow::activity_job::{ToolAllowlistError, validate_activity_tool_allowlist};

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
            tools,
            proc_allowed_programs,
            ..AgentLoopSpec::for_provider("test".to_string(), Provider::default(), 30)
        }),
    }
}
