//! [ORB-10709, ADR-0352] The exclusive workspace claim and the gate it puts on
//! workflow dispatch.
//!
//! The gate is asserted against the shared submission path itself, with no HTTP
//! or protocol adapter in the picture — that placement is the whole point, and a
//! test that went through one surface would not prove the others inherit it.

use orbit_types::telemetry::AuditEventStatus;
use serde_json::json;

use crate::OrbitRuntime;
use crate::adapter::tool_host::test_support::{
    run_tool_as_operator, test_runtime, unmanaged_tool_env_guard,
};
use crate::application::task::TaskAddParams;
use crate::application::workflow::{CompletionPolicy, ShipMode};

/// Acquire the claim and return its token.
fn acquire_claim(runtime: &OrbitRuntime, actor: &str) -> String {
    let result = run_tool_as_operator(
        runtime,
        "orbit.workspace.claim.acquire",
        json!({ "model": actor, "machine_id": "machine-1", "session_id": "session-1" }),
    )
    .expect("acquire workspace claim");
    assert_eq!(result["acquired"], json!(true));
    result["claim_token"]
        .as_str()
        .expect("claim grant carries a token")
        .to_string()
}

fn add_backlog_task(runtime: &OrbitRuntime) -> String {
    runtime
        .add_task(TaskAddParams {
            title: "Workspace claim fixture".to_string(),
            description: "A task selected by a workspace-claim test.".to_string(),
            ..Default::default()
        })
        .expect("create backlog task")
        .id
}

#[test]
fn a_refused_dispatch_is_recorded_as_denied_without_the_holders_token() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, _repo) = test_runtime();
    let token = acquire_claim(&runtime, "claude");
    let task_id = add_backlog_task(&runtime);

    let _ = runtime.submit_ship_run(
        ShipMode::Local,
        Some("main"),
        std::slice::from_ref(&task_id),
        CompletionPolicy::Review,
        &[],
        Some("test"),
        None,
        orbit_types::workflow::JobRunTrigger::cli(),
    );

    let events = runtime
        .list_audit_events(None, None, None, None, 200)
        .expect("read audit events");
    let denial = events
        .iter()
        .find(|event| event.command == "workspace.claim.dispatch.denied")
        .expect("a refused dispatch is audited");
    assert_eq!(denial.status, AuditEventStatus::Denied);
    let arguments = denial.arguments_json.as_deref().unwrap_or_default();
    assert!(
        arguments.contains("orbit.workflow.ship"),
        "the denial names the refused operation: {arguments}"
    );
    assert!(
        !arguments.contains(&token),
        "an audit reader is not the holder; the token must never be recorded"
    );
}
