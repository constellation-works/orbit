//! Sibling tests for `runtime/authorization.rs` — the capability chokepoint
//! [ORB-10453].
//!
//! These assert only on outcomes that a session context determines, never on
//! outcomes that ambient process state determines. Session grants outrank every
//! process signal, so a test that supplies them is deterministic whether it runs
//! under CI, under a managed Orbit run, or from a developer's terminal. The
//! process-signal precedence rules themselves are exercised as pure data in
//! `orbit_common::governance::authorization`.

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_store::contracts::TaskCreateParams;
use orbit_tools::ToolContext;
use orbit_types::policy::Role;
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde_json::json;

use crate::OrbitRuntime;

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
    runtime
        .stores()
        .task_records()
        .create(TaskCreateParams {
            actor: "test".to_string(),
            parent_id: None,
            title: "Governed operation fixture".to_string(),
            description: "Exercise the capability chokepoint".to_string(),
            acceptance_criteria: Vec::new(),
            dependencies: Vec::new(),
            relations: Vec::new(),
            tags: Vec::new(),
            required_tools: Vec::new(),
            plan: String::new(),
            execution_summary: String::new(),
            context_files: Vec::new(),
            repo_root: None,
            created_by: Some("test".to_string()),
            planned_by: None,
            implemented_by: None,
            status: TaskStatus::Backlog,
            priority: TaskPriority::Medium,
            complexity: None,
            task_type: TaskType::Chore,
            external_refs: Vec::new(),
            source_task_id: None,
            crew: None,
            orchestrator: None,
            comments: Vec::new(),
        })
        .expect("create task")
        .id
}

#[test]
fn an_agent_session_cannot_delete_a_task_through_any_tool_path() {
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
fn an_operator_session_reaches_the_same_operation() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task_id = seed_task(&runtime);

    runtime
        .run_tool_with_context_and_role(
            "orbit.task.delete",
            json!({ "id": task_id, "force": true }),
            Role::Admin,
            context_with([McpCapability::Operator]),
        )
        .expect("operator capability performs the governed operation");
}

#[test]
fn a_run_retains_the_destruction_it_dispatches() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    // `release_locks` is reached by the run's own deterministic dispatcher,
    // which stamps `Runner` onto its tool context. An unknown reservation is a
    // structured `released: false`, not a capability denial.
    let released = runtime
        .run_tool_with_context_and_role(
            "orbit.task.locks.release",
            json!({ "reservation_id": "reservation-no-such-reservation" }),
            Role::Admin,
            context_with([McpCapability::Runner]),
        )
        .expect("runner capability performs run-sanctioned destruction");
    assert_eq!(released["released"], json!(false));

    // The same grant does not widen into unrelated destruction.
    assert!(matches!(
        runtime.run_tool_with_context_and_role(
            "orbit.task.delete",
            json!({ "id": "ORB-00001", "force": true }),
            Role::Admin,
            context_with([McpCapability::Runner]),
        ),
        Err(OrbitError::CapabilityDenied(_))
    ));
}

#[test]
fn ungoverned_tools_are_untouched() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task_id = seed_task(&runtime);

    runtime
        .run_tool_with_context_and_role(
            "orbit.task.show",
            json!({ "id": task_id }),
            Role::Admin,
            context_with([McpCapability::Agent]),
        )
        .expect("an ungoverned tool is unaffected by the chokepoint");
}

#[test]
fn ungoverned_commands_pass_the_cli_chokepoint() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    runtime
        .authorize_command_operation("workspace", "list")
        .expect("a read-only command is not governed");
}

#[test]
fn a_denial_is_recorded_as_denied_not_failed() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task_id = seed_task(&runtime);

    let _ = runtime.run_tool_with_context_and_role(
        "orbit.task.delete",
        json!({ "id": task_id, "force": true }),
        Role::Admin,
        context_with([McpCapability::Agent]),
    );

    let events = runtime
        .list_audit_events(None, None, Some(AuditEventStatus::Denied), None, 50)
        .expect("list audit events");
    let record = events
        .iter()
        .find(|event| event.command == "authorization")
        .expect("the decision persists its own audit row");

    assert_eq!(record.target_type.as_deref(), Some("operation"));
    assert_eq!(record.target_id.as_deref(), Some("orbit.task.delete"));
    assert_eq!(record.status, AuditEventStatus::Denied);
    assert_eq!(
        record.effective_capabilities,
        BTreeSet::from([McpCapability::Agent]),
        "the audit row records what the caller actually held"
    );
    assert!(
        record
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("ORBIT_OPERATOR")),
        "the recorded denial names the escape hatch: {:?}",
        record.error_message
    );
}

/// [ORB-12257]: a denied `orbit.task.locks.release` — the operation named in
/// the bug report's 1,727 unexplained denials — must write an audit row that
/// actually explains itself: the operation name, the denial message, the
/// resolved role, and the capability the caller was missing.
#[test]
fn a_denied_task_locks_release_records_operation_message_role_and_capability() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    let error = runtime
        .run_tool_with_context_and_role(
            "orbit.task.locks.release",
            json!({ "reservation_id": "reservation-no-such-reservation" }),
            Role::Admin,
            context_with([McpCapability::Agent]),
        )
        .expect_err("an agent holds neither operator nor runner");
    assert!(
        matches!(error, OrbitError::CapabilityDenied(_)),
        "expected a capability denial, got: {error}"
    );

    let events = runtime
        .list_audit_events(None, None, Some(AuditEventStatus::Denied), None, 50)
        .expect("list audit events");
    let record = events
        .iter()
        .find(|event| event.command == "authorization")
        .expect("the decision persists its own audit row");

    // Operation name. `tool_name` stays unset on this row: the tool-dispatch
    // chokepoint that runs the authorization check already wrote its own
    // entry-point row with `tool_name` set, and `target_id` is this row's
    // operation-name column (see `record_authorization_event`).
    assert_eq!(
        record.target_id.as_deref(),
        Some("orbit.task.locks.release")
    );
    // Denial message.
    let message = record
        .error_message
        .as_deref()
        .expect("a denied row carries the denial message");
    assert!(message.contains("orbit.task.locks.release"), "{message}");
    assert!(
        message.contains("operator") || message.contains("runner"),
        "{message}"
    );
    // Resolved role.
    assert!(!record.role.is_empty());
    // Missing capability: the caller held `agent`, not the `operator`/`runner`
    // the operation required.
    assert_eq!(
        record.effective_capabilities,
        BTreeSet::from([McpCapability::Agent])
    );
}

/// [ORB-12257] `orbit audit stats` must break `denied` down by operation
/// rather than collapsing every refusal into one opaque total.
#[test]
fn audit_stats_denials_break_down_by_operation() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task_id = seed_task(&runtime);

    let _ = runtime.run_tool_with_context_and_role(
        "orbit.task.delete",
        json!({ "id": task_id, "force": true }),
        Role::Admin,
        context_with([McpCapability::Agent]),
    );
    let _ = runtime.run_tool_with_context_and_role(
        "orbit.task.locks.release",
        json!({ "reservation_id": "reservation-no-such-reservation" }),
        Role::Admin,
        context_with([McpCapability::Agent]),
    );
    let _ = runtime.run_tool_with_context_and_role(
        "orbit.task.locks.release",
        json!({ "reservation_id": "reservation-still-no-such-reservation" }),
        Role::Admin,
        context_with([McpCapability::Agent]),
    );

    let breakdown = runtime
        .audit_denials_by_operation(None)
        .expect("denials by operation");

    assert_eq!(
        breakdown
            .iter()
            .find(|(operation, _)| operation == "orbit.task.delete")
            .map(|(_, count)| *count),
        Some(1)
    );
    assert_eq!(
        breakdown
            .iter()
            .find(|(operation, _)| operation == "orbit.task.locks.release")
            .map(|(_, count)| *count),
        Some(2)
    );
}
