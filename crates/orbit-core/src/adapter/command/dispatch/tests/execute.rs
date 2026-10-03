//! Dispatching one tool call: audit rows, MCP session gating, and the
//! audit-write failure contract.

use orbit_common::OrbitError;
use orbit_store::Store;
use orbit_tools::ToolExecutionKind;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::{McpCapability, McpTransport, ToolSessionContext};
use std::collections::BTreeSet;
use std::sync::{Arc, Barrier};
use std::thread;

use serde_json::json;

use super::super::callback::override_activity_tools_for_test;
use super::super::execute::{
    ToolEntryPoint, finalize_successful_dispatch, take_tool_audit_recorded,
};
use crate::adapter::command::tests::support::{clear_identity_env, env_guard, fresh_runtime};

#[test]
fn dispatch_records_success_audit_with_mcp_subcommand_and_clamped_duration() {
    let _g = env_guard();
    let runtime = fresh_runtime();

    let outcome = runtime
        .execute_tool_command_dispatch(
            "orbit.search",
            json!({ "query": "anything", "model": orbit_common::test_fixtures::TEST_CODEX_MODEL }),
            None,
            None,
            ToolEntryPoint::Mcp,
        )
        .expect("dispatch ok");
    assert!(outcome.audit_recorded);

    let events = runtime
        .list_audit_events(None, Some("orbit.search".to_string()), None, None, 16)
        .expect("list audit events");
    assert_eq!(events.len(), 1, "exactly one audit row");
    let row = &events[0];
    assert_eq!(row.command, "tool");
    assert_eq!(row.subcommand.as_deref(), Some("run-mcp"));
    assert_eq!(row.tool_name.as_deref(), Some("orbit.search"));
    assert_eq!(row.target_type.as_deref(), Some("tool"));
    assert_eq!(row.target_id.as_deref(), Some("orbit.search"));
    assert_eq!(row.role, "unverified");
    assert_eq!(row.status, AuditEventStatus::Success);
    assert_eq!(row.exit_code, 0);
    assert!(
        row.duration_ms >= 1,
        "duration_ms clamped to >= 1 (got {})",
        row.duration_ms
    );
}

#[test]
fn runtime_dispatch_reuses_the_open_audit_store() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    let audit_db = runtime.context.persistence().audit_db.clone();
    let opened = Store::thread_file_open_count_for(&audit_db);

    let outcome = runtime
        .execute_tool_command_dispatch(
            "orbit.search",
            json!({ "query": "reuse", "model": orbit_common::test_fixtures::TEST_CODEX_MODEL }),
            None,
            None,
            ToolEntryPoint::Mcp,
        )
        .expect("dispatch ok");
    assert!(outcome.audit_recorded);

    let events = runtime
        .list_audit_events(None, Some("orbit.search".to_string()), None, None, 16)
        .expect("list audit events");
    assert_eq!(events.len(), 1, "exactly one audit row");
    assert_eq!(events[0].status, AuditEventStatus::Success);
    assert_eq!(
        Store::thread_file_open_count_for(&audit_db),
        opened,
        "runtime-backed dispatch must not reopen the audit database"
    );
}

pub(super) fn write_free_runtime(runtime: &crate::OrbitRuntime) -> crate::OrbitRuntime {
    let config = orbit_config::ResolvedConfig::load(&orbit_config::ConfigRoots::new(
        runtime.global_root(),
        runtime.shared_root(),
    ))
    .expect("fixture config");
    crate::OrbitRuntime::build_from_resolved_config_write_free(
        &runtime.global_root(),
        &runtime.shared_root(),
        &runtime.local_root(),
        runtime.workspace_runtime_binding().cloned(),
        &config,
        orbit_store::workflow::layout::LayoutUpgradeReport::default(),
        crate::runtime::HostLifetime::ShortLived,
    )
    .expect("write-free runtime")
}

#[test]
fn write_free_dispatch_persists_audit_without_widening_state_access() {
    if crate::application::run_isolated_test(std::any::type_name_of_val(
        &write_free_dispatch_persists_audit_without_widening_state_access,
    )) {
        return;
    }
    let _g = env_guard();
    let runtime = fresh_runtime();
    let reader = write_free_runtime(&runtime);
    assert!(reader.is_write_free());
    let store = reader.sqlite_store().expect("reader store");
    assert!(store.is_read_only());
    let migrations = store.applied_migrations().expect("migration ledger");

    let outcome = reader
        .execute_tool_command_dispatch(
            "orbit.task.list",
            json!({"limit": 1}),
            None,
            None,
            ToolEntryPoint::Cli,
        )
        .expect("read-only dispatch");
    assert!(outcome.audit_recorded);
    assert!(
        take_tool_audit_recorded(),
        "suppress the duplicate CLI audit"
    );
    let rows = reader
        .list_audit_events(None, Some("orbit.task.list".into()), None, None, 16)
        .expect("audit rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, AuditEventStatus::Success);
    assert_eq!(rows[0].subcommand.as_deref(), Some("run"));
    assert_eq!(store.applied_migrations().expect("ledger"), migrations);
    store
        .with_transaction(|tx| {
            tx.connection()
                .execute(
                    "INSERT INTO schema_meta(key, value, updated_at)
                     VALUES ('state-write-probe', 'probe', 'probe')",
                    [],
                )
                .map(|_| ())
                .map_err(|error| OrbitError::Store(error.to_string()))
        })
        .expect_err("the runtime's original state handle must still reject writes");
}

#[test]
fn dispatch_records_failure_audit_when_tool_handler_errors() {
    let _g = env_guard();
    let runtime = fresh_runtime();

    // Missing required input fields makes the task tool error out at
    // dispatch time. That gives us a deterministic dispatch-failure path
    // that runs through the runtime audit-write seam.
    let result = runtime.execute_tool_command_dispatch(
        "orbit.task.show",
        json!({}),
        None,
        None,
        ToolEntryPoint::Mcp,
    );
    assert!(result.is_err(), "dispatch errors with missing input");

    let events = runtime
        .list_audit_events(None, Some("orbit.task.show".to_string()), None, None, 16)
        .expect("list audit events");
    assert_eq!(events.len(), 1);
    let row = &events[0];
    assert_eq!(row.status, AuditEventStatus::Failure);
    assert_eq!(row.exit_code, 1);
    assert!(row.error_message.is_some());
    assert_eq!(row.subcommand.as_deref(), Some("run-mcp"));
}

fn mcp_context(capabilities: impl IntoIterator<Item = McpCapability>) -> ToolSessionContext {
    ToolSessionContext {
        transport: Some(McpTransport::Local),
        effective_capabilities: capabilities.into_iter().collect(),
        ..ToolSessionContext::default()
    }
}

#[test]
fn mcp_empty_session_is_denied_before_governed_tool_execution_and_audited() {
    let _g = env_guard();
    clear_identity_env();
    let runtime = fresh_runtime();
    let result = runtime
        .execute_tool_command_dispatch_with_session_context(
            "orbit.workflow.run.list",
            json!({}),
            None,
            None,
            ToolEntryPoint::Mcp,
            mcp_context([]),
        )
        .expect_err("an empty MCP session must fail closed");

    assert!(matches!(result, OrbitError::CapabilityDenied(_)));
    let events = runtime
        .list_audit_events(
            None,
            Some("orbit.workflow.run.list".to_string()),
            None,
            None,
            1,
        )
        .expect("read denied MCP audit row");
    let row = &events[0];
    assert_eq!(row.status, AuditEventStatus::Denied);
    assert_eq!(row.transport, Some(McpTransport::Local));
    assert!(row.effective_capabilities.is_empty());
}

#[test]
fn mcp_agent_session_is_denied_before_governed_tool_execution() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    let result = runtime
        .execute_tool_command_dispatch_with_session_context(
            "orbit.workflow.run.list",
            json!({}),
            None,
            None,
            ToolEntryPoint::Mcp,
            mcp_context([McpCapability::Agent]),
        )
        .expect_err("an agent MCP session must not reach operator tools");

    assert!(matches!(result, OrbitError::CapabilityDenied(_)));
    let events = runtime
        .list_audit_events(
            None,
            Some("orbit.workflow.run.list".to_string()),
            None,
            None,
            1,
        )
        .expect("read agent MCP audit row");
    let row = &events[0];
    assert_eq!(row.status, AuditEventStatus::Denied);
    assert_eq!(row.transport, Some(McpTransport::Local));
    assert_eq!(
        row.effective_capabilities,
        BTreeSet::from([McpCapability::Agent])
    );
}

#[test]
fn mcp_operator_session_reaches_governed_tool_execution() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    let result = runtime.execute_tool_command_dispatch_with_session_context(
        "orbit.workflow.run.list",
        json!({}),
        None,
        None,
        ToolEntryPoint::Mcp,
        mcp_context([McpCapability::Operator]),
    );

    assert!(result.is_ok(), "operator session was denied: {result:?}");
    let events = runtime
        .list_audit_events(
            None,
            Some("orbit.workflow.run.list".to_string()),
            None,
            None,
            1,
        )
        .expect("read operator MCP audit row");
    assert_eq!(events[0].status, AuditEventStatus::Success);
    assert_eq!(
        events[0].effective_capabilities,
        BTreeSet::from([McpCapability::Operator])
    );
}

#[test]
fn dispatch_records_failure_audit_when_identity_setup_rejects_pair() {
    let _g = env_guard();
    let runtime = fresh_runtime();

    // Inconsistent agent/model: `claude` family does not produce
    // `gpt-5.5`. `resolve_agent_identity` rejects this via
    // `normalize_agent_family_for_model`. The audit-write path must
    // still capture the failure — this is the gap that bypassed audit
    // before the closure-wrapping fix.
    let result = runtime.execute_tool_command_dispatch(
        "orbit.search",
        json!({ "query": "anything" }),
        Some("claude".to_string()),
        Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        ToolEntryPoint::Cli,
    );
    assert!(result.is_err(), "identity rejection propagates");

    let events = runtime
        .list_audit_events(None, Some("orbit.search".to_string()), None, None, 16)
        .expect("list audit events");
    assert_eq!(
        events.len(),
        1,
        "setup failure produced exactly one audit row"
    );
    let row = &events[0];
    assert_eq!(row.status, AuditEventStatus::Failure);
    assert_eq!(row.exit_code, 1);
    assert_eq!(row.subcommand.as_deref(), Some("run"));
    assert!(row.error_message.is_some(), "error message captured");
}

#[test]
fn cli_entry_point_records_run_subcommand() {
    let _g = env_guard();
    let runtime = fresh_runtime();

    runtime
        .execute_tool_command(
            "orbit.search",
            json!({ "query": "anything", "model": orbit_common::test_fixtures::TEST_CODEX_MODEL }),
            None,
            None,
        )
        .expect("dispatch ok");

    let events = runtime
        .list_audit_events(None, Some("orbit.search".to_string()), None, None, 16)
        .expect("list audit events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].subcommand.as_deref(), Some("run"));
}

#[test]
fn managed_agent_activity_allowlist_denies_an_omitted_tool() {
    let runtime = crate::OrbitRuntime::in_memory().expect("build in-memory runtime");
    let _activity_tools = override_activity_tools_for_test(["orbit.search"]);

    let error = runtime
        .execute_tool_command(
            "orbit.task.show",
            json!({ "id": "ORB-00001" }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect_err("an omitted managed-agent tool must remain denied");

    assert!(matches!(error, OrbitError::PolicyDenied(_)), "{error:?}");
}

#[test]
fn concurrent_tool_dispatch_writes_distinct_execution_ids() {
    let _g = env_guard();
    let runtime = Arc::new(fresh_runtime());
    let workers = 8;
    let barrier = Arc::new(Barrier::new(workers));

    let handles: Vec<_> = (0..workers)
        .map(|_| {
            let runtime = Arc::clone(&runtime);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                runtime
                    .execute_tool_command_dispatch(
                        "orbit.search",
                        json!({ "query": "anything", "model": orbit_common::test_fixtures::TEST_CODEX_MODEL }),
                        None,
                        None,
                        ToolEntryPoint::Cli,
                    )
                    .expect("dispatch ok");
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("worker joined");
    }

    let events = runtime
        .list_audit_events(None, Some("orbit.search".to_string()), None, None, workers)
        .expect("list audit events");
    let execution_ids: BTreeSet<_> = events.iter().map(|event| &event.execution_id).collect();

    assert_eq!(events.len(), workers);
    assert_eq!(execution_ids.len(), workers);
}

#[test]
fn dedup_signal_is_set_after_dispatch_and_cleared_on_take() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    let _ = take_tool_audit_recorded();
    assert!(!take_tool_audit_recorded(), "starts clear");

    runtime
        .execute_tool_command_dispatch(
            "orbit.search",
            json!({ "query": "anything" }),
            None,
            None,
            ToolEntryPoint::Cli,
        )
        .expect("dispatch ok");

    assert!(
        take_tool_audit_recorded(),
        "runtime sets flag after audit write"
    );
    assert!(
        !take_tool_audit_recorded(),
        "take is one-shot and resets the flag"
    );
}

#[test]
fn successful_dispatch_returns_value_when_audit_persists() {
    let outcome = finalize_successful_dispatch(
        "orbit.task.update",
        ToolExecutionKind::Mutating,
        json!({"ok": true}),
        Ok(()),
    )
    .expect("audit persisted -> success");

    assert!(outcome.audit_recorded);
    assert_eq!(outcome.value, json!({"ok": true}));
}

#[test]
fn successful_mutation_fails_when_audit_row_cannot_be_persisted() {
    // A mutating tool completed (value present), but the audit write
    // failed. Finding M1: the call must fail rather than surface a
    // successful, un-audited mutation.
    let audit_write = Err(OrbitError::Store("disk full".to_string()));
    let result = finalize_successful_dispatch(
        "orbit.task.update",
        ToolExecutionKind::Mutating,
        json!({"mutated": true}),
        audit_write,
    );

    let err = result.expect_err("un-audited mutation must fail the call");
    let message = err.to_string();
    assert!(
        message.contains("orbit.task.update") && message.contains("audit row"),
        "error names the tool and the missing audit row: {message}"
    );
}

#[test]
fn successful_read_only_dispatch_survives_unwritable_audit_store() {
    let value = json!({"items": []});
    let outcome = finalize_successful_dispatch(
        "orbit.task.list",
        ToolExecutionKind::ReadOnly,
        value.clone(),
        Err(OrbitError::Store(
            "attempt to write a readonly database".to_string(),
        )),
    )
    .expect("passive telemetry must not change a read-only tool result");

    assert_eq!(outcome.value, value);
    assert!(!outcome.audit_recorded);
}

/// The managed-agent envelope a deny-mode orchestrator stamps, applied under
/// the same process-wide lock [`env_guard`] takes.
fn deny_mode_managed_env(disallow_list: &str) -> orbit_common::test_env::ScopedEnv {
    orbit_common::test_env::scoped([
        ("ORBIT_AGENT_NAME", None),
        ("ORBIT_AGENT_MODEL", None),
        ("ORBIT_TASK_ID", None),
        ("ORBIT_ACTIVITY_ID", None),
        ("ORBIT_STEP_INDEX", None),
        ("ORBIT_ALLOWED_TOOLS", None),
        ("ORBIT_SESSION_ID", None),
        ("ORBIT_RUN_ID", Some("jrun-deny-mode")),
        (crate::adapter::command::dispatch::ORBIT_PLUGIN_ENV, None),
        (orbit_tools::plugin::ORBIT_PLUGIN_CALLBACK_ENV, None),
        (orbit_tools::plugin::ORBIT_PLUGIN_CALLBACK_FD_ENV, None),
        (
            crate::runtime::run_input::ORBIT_MANAGED_RUN_CONTEXT_ENV,
            Some("1"),
        ),
        ("ORBIT_TASK_ACTOR_KIND", Some("agent")),
        // The concrete set an older MCP server would enforce instead.
        ("ORBIT_ACTIVITY_TOOLS", Some("orbit.search")),
        ("ORBIT_ACTIVITY_TOOL_POLICY", Some("deny")),
        ("ORBIT_ACTIVITY_TOOLS_DENY", Some(disallow_list)),
        ("ORBIT_ACTIVITY_NAME", Some("custom_agent")),
    ])
}

/// [ORB-13315] Through the managed MCP entry point (the path Claude and
/// Codex agents reach), deny mode refuses a listed tool with a PolicyDenied
/// naming the disallow list and records the refusal as today's audit row.
#[test]
fn managed_mcp_deny_mode_refuses_a_disallowed_tool_and_audits_it() {
    let _env = deny_mode_managed_env("orbit.task.*,orbit.workflow.ship");
    let runtime = fresh_runtime();

    let error = runtime
        .execute_tool_command_dispatch(
            "orbit.task.show",
            json!({ "id": "ORB-00001" }),
            None,
            None,
            ToolEntryPoint::Mcp,
        )
        .expect_err("a disallowed tool must be refused");
    match &error {
        OrbitError::PolicyDenied(message) => assert_eq!(
            message,
            "tool 'orbit.task.show' is in the activity disallow list (custom_agent)"
        ),
        other => panic!("expected PolicyDenied, got {other:?}"),
    }

    let events = runtime
        .list_audit_events(None, Some("orbit.task.show".to_string()), None, None, 16)
        .expect("list audit events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].status, AuditEventStatus::Denied);
    assert_eq!(events[0].subcommand.as_deref(), Some("run-mcp"));
    assert!(
        events[0]
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("activity disallow list")),
        "{:?}",
        events[0].error_message
    );
}

/// [ORB-13315] Deny mode leaves every other registered tool callable — not
/// just the stamped legacy allowlist an older server would enforce.
#[test]
fn managed_mcp_deny_mode_allows_every_tool_not_on_the_disallow_list() {
    let _env = deny_mode_managed_env("orbit.workflow.ship");
    let runtime = fresh_runtime();

    for (tool, input) in [
        ("orbit.search", json!({ "query": "anything" })),
        ("orbit.task.list", json!({})),
    ] {
        runtime
            .execute_tool_command_dispatch(tool, input, None, None, ToolEntryPoint::Mcp)
            .unwrap_or_else(|error| panic!("{tool} must stay callable: {error:?}"));
    }
}
