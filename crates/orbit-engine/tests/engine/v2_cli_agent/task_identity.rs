//! CLI dispatch reserves the task-identity names at the provider boundary: an
//! explicitly admitted value (`pass` or a crew `env` mapping) is cleared for an
//! unbound activity and replaced by the activity's own stamp for a bound one.
use super::*;
use orbit_common::security::child_env::TASK_IDENTITY_ENV_VARS;

/// Dispatch one activity whose host admits an outer value for every reserved
/// name, and return the `NAME=value` lines the provider observed.
fn observed_identity(run_id: &str, input: Value) -> Vec<String> {
    let tmp = tempfile::tempdir().unwrap();
    let (writer, _store) = build_writer(tmp.path(), run_id).unwrap();
    let observed = tmp.path().join("observed-env");
    let probe: String = TASK_IDENTITY_ENV_VARS
        .iter()
        .map(|name| {
            format!(
                "printf '{name}=%s\\n' \"${{{name}-unset}}\" >> '{}'\n",
                observed.display()
            )
        })
        .collect();
    let fake = fake_cli(
        "claude",
        &format!(
            "#!/bin/sh\ncat > /dev/null\n{probe}echo '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{}},\"error\":null}}'\n"
        ),
    )
    .unwrap();
    let mut host = ScriptHost::new(fake.cli_path());
    host.admitted_env = TASK_IDENTITY_ENV_VARS
        .iter()
        .map(|name| ((*name).to_string(), "outer-task".to_string()))
        .collect();
    let outcome = dispatch_v2_activity(V2DispatchInput {
        activity_name: "cli_task_identity",
        spec: &ActivityV2Spec::AgentLoop(cli_agent_loop_spec(None)),
        fs_profile: None,
        input,
        audit: writer,
        run_id,
        host: Some(&host),
    })
    .unwrap();
    assert!(outcome.success, "fake claude should exit 0");
    fs::read_to_string(&observed)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn unbound_activity_receives_no_admitted_task_identity() {
    let seen = observed_identity(
        "task-identity-unbound",
        serde_json::json!({"prompt": "hello"}),
    );
    assert_eq!(
        seen,
        [
            "ORBIT_TASK_ID=unset",
            "ORBIT_ACTIVE_TASK_ID=unset",
            "AGENT_TASK=unset"
        ],
        "an admitted outer value must not bind an unbound activity"
    );
}

#[test]
fn bundled_activity_receives_no_admitted_task_identity() {
    let seen = observed_identity(
        "task-identity-bundle",
        serde_json::json!({"prompt": "hello", "task_ids": ["ORB-1", "ORB-2"]}),
    );
    assert!(seen.iter().all(|line| line.ends_with("=unset")), "{seen:?}");
}

#[test]
fn bound_activity_receives_its_own_task_identity() {
    let seen = observed_identity(
        "task-identity-bound",
        serde_json::json!({"prompt": "hello", "task_id": "ORB-1"}),
    );
    assert_eq!(
        seen,
        [
            "ORBIT_TASK_ID=ORB-1",
            "ORBIT_ACTIVE_TASK_ID=ORB-1",
            "AGENT_TASK=ORB-1"
        ],
        "the activity's stamp replaces every admitted outer value"
    );
}
