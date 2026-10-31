//! Sibling tests for `runtime/authorization.rs` — the capability chokepoint
//! [ORB-10453].
//!
//! These assert only on outcomes that a session context determines, never on
//! outcomes that ambient process state determines. Session grants outrank every
//! process signal, so a test that supplies them is deterministic whether it runs
//! under CI, under a managed Orbit run, or from a developer's terminal. The
//! process-signal precedence rules themselves are exercised as pure data in
//! `orbit_common::governance::authorization`.

use orbit_engine::RuntimeHost;

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_tools::ToolContext;
use orbit_types::policy::Role;
use orbit_types::task::TaskStatus;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde_json::json;

use crate::task_params_test_support::task_create_params;
use orbit_core::OrbitRuntime;

fn context_with(capabilities: [McpCapability; 1]) -> ToolContext {
    ToolContext {
        session_context: ToolSessionContext {
            effective_capabilities: BTreeSet::from(capabilities),
            ..ToolSessionContext::default()
        },
        ..ToolContext::default()
    }
}

fn seed_task(runtime: &OrbitRuntime) -> String {
    crate::admission_audit::store_support::tasks(runtime)
        .create_task(task_create_params(
            "Governed operation fixture",
            "Exercise the capability chokepoint",
            TaskStatus::Backlog,
        ))
        .expect("create task")
        .id
}

#[test]
fn an_agent_session_cannot_delete_a_task_through_any_tool_path() {
    if !crate::admission_audit::isolate(
        module_path!(),
        "an_agent_session_cannot_delete_a_task_through_any_tool_path",
    ) {
        return;
    }

    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task_id = seed_task(&runtime);

    let error = runtime
        .run_tool_with_context_and_role(
            "orbit.task.delete",
            json!({ "id": task_id, "force": true }),
            Role::Admin,
            context_with([McpCapability::Agent]),
        )
        .expect_err("agent capability must not reach task deletion");

    match error {
        OrbitError::CapabilityDenied(message) => {
            assert!(message.contains("orbit.task.delete"), "{message}");
            assert!(message.contains("operator"), "{message}");
            assert!(message.contains("ORBIT_OPERATOR"), "{message}");
        }
        other => panic!("expected a capability denial, got: {other}"),
    }

    // The refusal is real, not cosmetic: the task is still there.
    assert!(runtime.get_task(&task_id).is_ok());
}

#[test]
fn grant_changes_and_denied_calls_share_the_authorization_audit_predicate() {
    if !crate::admission_audit::isolate(
        module_path!(),
        "grant_changes_and_denied_calls_share_the_authorization_audit_predicate",
    ) {
        return;
    }
    let (_root, runtime, _repo) = crate::admission_audit::tool_support::test_runtime();
    let task = runtime
        .add_task(orbit_core::application::task::TaskAddParams {
            title: "Audit grant".into(),
            ..Default::default()
        })
        .unwrap();
    runtime
        .run_tool_with_context_and_role(
            "orbit.task.update",
            json!({"id": task.id, "granted_tools": ["orbit.auto_task.update"]}),
            Role::Admin,
            context_with([McpCapability::Operator]),
        )
        .unwrap();
    let events = runtime
        .list_audit_events(None, None, None, None, 200)
        .unwrap();
    assert!(
        events.iter().any(|event| event.command == "authorization"
            && event.target_id.as_deref() == Some("orbit.task.grant_tools")
            && event.status == orbit_core::AuditEventStatus::Success),
        "{events:#?}"
    );
    let mut context = context_with([McpCapability::Agent]);
    context.tool_deny_policy = Some(orbit_types::workflow::ActivityToolDenyPolicy {
        activity: "agent_implement".into(),
        disallow_list: vec!["orbit.auto_task.update".into()],
    });
    let error = runtime
        .run_tool_with_context_and_role(
            "orbit.auto_task.update",
            json!({"name": "never-applied"}),
            Role::Admin,
            context,
        )
        .expect_err("a mutating grant must remain denied live");
    assert!(error.to_string().contains("disallow list"), "{error}");
    let events = runtime
        .list_audit_events(None, None, None, None, 200)
        .unwrap();
    assert!(
        events.iter().any(|event| event.command == "authorization"
            && event.target_id.as_deref() == Some("orbit.auto_task.update")
            && event.status == orbit_core::AuditEventStatus::Denied),
        "{events:#?}"
    );
    let denials = runtime.audit_denials_by_operation(None).unwrap();
    assert_eq!(denials, vec![("orbit.auto_task.update".to_string(), 1)]);
    assert_eq!(
        runtime.get_task(&task.id).unwrap().granted_tools,
        ["orbit.auto_task.update"]
    );
}
