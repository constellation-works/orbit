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
use orbit_common::test_env::{self, AGENT_IDENTITY_ENV};
use orbit_store::contracts::TaskCreateParams;
use orbit_tools::ToolContext;
use orbit_types::policy::Role;
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
use orbit_types::telemetry::{AuditEvent, AuditEventStatus};
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde_json::json;

use crate::{ActorIdentity, OrbitRuntime};

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

/// [ORB-12274]: a bare CLI caller with `USER` set must record audit `role`
/// as the actor kind (`human`), never `human:<os-user>`. The same identity
/// must not fragment `audit_denials_by_role`.
#[test]
fn authorization_row_role_for_bare_cli_is_human_not_os_user() {
    let os_user = "qa-operator";
    let _env = test_env::scoped(AGENT_IDENTITY_ENV.iter().map(|name| (*name, None)).chain([
        ("ORBIT_OPERATOR", None),
        ("USER", Some(os_user)),
        ("USERNAME", None),
        ("LOGNAME", None),
    ]));

    let actor = ActorIdentity::from_env();
    assert_eq!(actor.label, format!("human:{os_user}"));

    let runtime = deny_governed_tool_as(actor);
    let record = authorization_record(&runtime, AuditEventStatus::Denied);

    assert_eq!(record.role, "human");
    assert!(
        !record.role.contains(os_user),
        "authorization role must not carry the OS account name: {}",
        record.role
    );

    let buckets = runtime
        .audit_denials_by_role(None)
        .expect("denials by role");
    assert!(
        buckets.iter().any(|(role, _)| role == "human"),
        "expected a human denial bucket, got {buckets:?}"
    );
    assert!(
        buckets
            .iter()
            .all(|(role, _)| !role.contains(os_user) && !role.starts_with("human:")),
        "denial-by-role buckets must be actor kinds, not OS accounts: {buckets:?}"
    );
}

/// [ORB-12274]: when the operator override is the resolved identity, the
/// authorization row records `operator`, not a named human or OS account.
#[test]
fn authorization_row_role_for_operator_override_identity_is_operator() {
    let _env = test_env::scoped(AGENT_IDENTITY_ENV.iter().map(|name| (*name, None)).chain([
        ("ORBIT_OPERATOR", Some("1")),
        ("USER", Some("qa-operator")),
        ("USERNAME", None),
        ("LOGNAME", None),
    ]));

    let actor = ActorIdentity::from_env();
    assert_eq!(actor.label, "operator");

    let runtime = deny_governed_tool_as(actor);
    let record = authorization_record(&runtime, AuditEventStatus::Denied);
    assert_eq!(record.role, "operator");
}

/// [ORB-12274]: an agent envelope still records the canonical family as
/// authorization `role`.
#[test]
fn authorization_row_role_for_agent_envelope_is_canonical_family() {
    let _env = test_env::scoped(
        AGENT_IDENTITY_ENV
            .iter()
            .filter(|name| **name != "ORBIT_AGENT_MODEL")
            .map(|name| (*name, None))
            .chain([
                ("ORBIT_AGENT_MODEL", Some("grok")),
                ("ORBIT_OPERATOR", None),
                ("USER", Some("qa-operator")),
            ]),
    );

    let actor = ActorIdentity::from_env();
    assert_eq!(actor.label, "grok");

    let runtime = deny_governed_tool_as(actor);
    let record = authorization_record(&runtime, AuditEventStatus::Denied);
    assert_eq!(record.role, "grok");
}

fn deny_governed_tool_as(actor: ActorIdentity) -> OrbitRuntime {
    let runtime = OrbitRuntime::in_memory()
        .expect("build runtime")
        .with_actor(actor);
    let task_id = seed_task(&runtime);
    let error = runtime
        .run_tool_with_context_and_role(
            "orbit.task.delete",
            json!({ "id": task_id, "force": true }),
            Role::Admin,
            context_with([McpCapability::Agent]),
        )
        .expect_err("agent capability must not reach task deletion");
    assert!(
        matches!(error, OrbitError::CapabilityDenied(_)),
        "expected a capability denial, got: {error}"
    );
    runtime
}

fn authorization_record(runtime: &OrbitRuntime, status: AuditEventStatus) -> AuditEvent {
    runtime
        .list_audit_events(None, None, Some(status), None, 50)
        .expect("list audit events")
        .into_iter()
        .find(|event| event.command == "authorization")
        .expect("the decision persists its own audit row")
}
