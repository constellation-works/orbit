use orbit_types::task::ExecutionLocation;
use orbit_types::tool::{WorkerInvocation, tool_requirement_name};

fn binding() -> WorkerInvocation {
    WorkerInvocation {
        owner_machine_id: "owner".into(),
        owner_workspace_id: "workspace".into(),
        owner_destination: "owner/workspace".into(),
        task_id: "task".into(),
        claim_id: "claim".into(),
        execution: ExecutionLocation {
            machine_id: "executor".into(),
            machine_name: None,
        },
        bound_run_id: "run".into(),
    }
}

#[test]
fn arguments_cannot_erase_or_replace_bound_identity() {
    let binding = binding();
    assert!(binding.validate_arguments(&serde_json::json!({})).is_ok());
    for key in ["task_id", "during_task", "claim_id", "bound_run_id"] {
        for value in [
            serde_json::Value::Null,
            serde_json::json!("other"),
            serde_json::json!(42),
        ] {
            assert!(
                binding
                    .validate_arguments(&serde_json::json!({key: value}))
                    .is_err()
            );
        }
    }
}

#[test]
fn ordinary_session_json_cannot_introduce_worker_authority() {
    let session: orbit_types::tool::ToolSessionContext = serde_json::from_value(
        serde_json::json!({"worker_invocation": binding(), "worker_host_call": true}),
    )
    .unwrap();
    assert!(session.worker_invocation.is_none());
    assert!(!session.worker_host_call);
}

#[test]
fn a_requirement_stored_before_tool_families_reads_as_its_family_action() {
    // Tasks minted and auto-task definitions written before ORB-23280 name
    // the operation; admission reads it as the family action.
    assert_eq!(
        tool_requirement_name("orbit.task.artifact.put"),
        "orbit.task.artifact:put"
    );
    assert_eq!(
        tool_requirement_name("orbit.task.artifact.get"),
        "orbit.task.artifact:get"
    );
    for unchanged in ["orbit.task.artifact:put", "orbit.task.show", "orbit.search"] {
        assert_eq!(tool_requirement_name(unchanged), unchanged);
    }
}
