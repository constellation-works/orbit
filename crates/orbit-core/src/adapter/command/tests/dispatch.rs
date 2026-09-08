use orbit_common::OrbitError;
use orbit_store::Store;
use orbit_tools::ToolExecutionKind;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::{McpCapability, McpTransport, ToolSessionContext};
use std::collections::BTreeSet;
use std::sync::{Arc, Barrier};
use std::thread;

use serde_json::json;

use super::support::{clear_identity_env, env_guard, fresh_runtime, set_identity_env};
use crate::adapter::command::dispatch::{
    ORBIT_MANAGED_RUN_CONTEXT_ENV, ToolEntryPoint, audit_role_label,
    audit_role_label_for_entry_point, finalize_successful_dispatch,
    override_activity_tools_for_test, reservation_owner_from_env, resolve_audit_context,
    take_tool_audit_recorded, trusted_mcp_audit_context,
};

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
fn audit_role_label_prefers_input_json_over_flags_and_env() {
    let _g = env_guard();
    // Set env vars to a value we never expect to see, so a leak surfaces
    // as a test failure with a recognizable string.
    set_identity_env("env-leak", "env-leak-model");
    let role = audit_role_label(
        &json!({ "agent": "claude", "model": "opus-4.6" }),
        Some("codex"),
        Some(orbit_common::test_fixtures::TEST_CODEX_MODEL),
    );
    clear_identity_env();
    assert_eq!(role, "opus-4.6");
}

#[test]
fn audit_role_label_prefers_flags_over_env_when_input_absent() {
    let _g = env_guard();
    set_identity_env("env-leak", "env-leak-model");
    let role = audit_role_label(
        &json!({ "query": "x" }),
        Some("codex"),
        Some(orbit_common::test_fixtures::TEST_CODEX_MODEL),
    );
    clear_identity_env();
    assert_eq!(role, orbit_common::test_fixtures::TEST_CODEX_MODEL);
}

#[test]
fn audit_role_label_falls_back_to_env_when_input_and_flags_absent() {
    let _g = env_guard();
    set_identity_env("claude", "opus-4.6");
    let role = audit_role_label(&json!({ "query": "x" }), None, None);
    clear_identity_env();
    assert_eq!(role, "claude");
}

#[test]
fn audit_role_label_overwrites_self_reported_model_with_env_family() {
    let _g = env_guard();
    set_identity_env("claude", orbit_common::test_fixtures::TEST_CLAUDE_MODEL);
    let role = audit_role_label(&json!({ "model": "opus-4.7" }), None, None);
    clear_identity_env();
    assert_eq!(role, "claude");
}

#[test]
fn cli_tool_dispatch_env_identity_overwrites_task_update_self_reported_model() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    let task = runtime
        .add_task(crate::application::task::TaskAddParams {
            title: "identity regression".to_string(),
            description: "exercise CLI tool identity overwrite".to_string(),
            acceptance_criteria: vec!["implemented_by is canonical".to_string()],
            plan: "Do the work.".to_string(),
            status: Some(orbit_types::task::TaskStatus::InProgress),
            ..Default::default()
        })
        .expect("seed in-progress task");
    set_identity_env("grok", "grok-build");

    runtime
        .execute_tool_command_dispatch(
            "orbit.task.update",
            json!({
                "id": task.id.clone(),
                "status": "review",
                "execution_summary": "Done.",
                "model": orbit_common::test_fixtures::TEST_CLAUDE_MODEL
            }),
            None,
            None,
            ToolEntryPoint::Cli,
        )
        .expect("task update succeeds");
    clear_identity_env();

    let updated = runtime.get_task(&task.id).expect("read updated task");
    assert_eq!(updated.implemented_by.as_deref(), Some("grok"));
}

#[test]
fn audit_role_label_defaults_to_agent_when_no_identity_available() {
    let _g = env_guard();
    clear_identity_env();
    let role = audit_role_label(&json!({}), None, None);
    assert_eq!(role, "agent");
}

fn clear_audit_context_env() {
    // SAFETY: tests serialize through `env_guard()` before calling this.
    unsafe {
        std::env::remove_var("ORBIT_TASK_ID");
        std::env::remove_var("ORBIT_RUN_ID");
        std::env::remove_var(ORBIT_MANAGED_RUN_CONTEXT_ENV);
        std::env::remove_var("ORBIT_ACTIVITY_ID");
        std::env::remove_var("ORBIT_STEP_INDEX");
    }
}

fn set_audit_context_env(task: &str, run: &str, activity: &str, step: &str) {
    // SAFETY: tests serialize through `env_guard()` before calling this.
    unsafe {
        std::env::set_var("ORBIT_TASK_ID", task);
        std::env::set_var("ORBIT_RUN_ID", run);
        std::env::set_var("ORBIT_ACTIVITY_ID", activity);
        std::env::set_var("ORBIT_STEP_INDEX", step);
    }
}

#[test]
fn audit_context_input_wins_over_env() {
    let _g = env_guard();
    set_audit_context_env("env-task", "env-run", "env-activity", "9");
    let ctx = resolve_audit_context(
        &json!({
            "task_id": "T-input",
            "job_run_id": "jrun-input",
            "activity_id": "act-input",
            "step_index": 3,
        }),
        ToolEntryPoint::Cli,
        None,
    );
    clear_audit_context_env();

    assert_eq!(ctx.task_id.as_deref(), Some("T-input"));
    assert_eq!(ctx.job_run_id.as_deref(), Some("jrun-input"));
    assert_eq!(ctx.activity_id.as_deref(), Some("act-input"));
    assert_eq!(ctx.step_index, Some(3));
}

#[test]
fn audit_context_falls_back_to_env_when_input_absent() {
    let _g = env_guard();
    set_audit_context_env("T20260428-7", "jrun-from-env", "agent_implement", "2");
    let ctx = resolve_audit_context(&json!({}), ToolEntryPoint::Cli, None);
    clear_audit_context_env();

    assert_eq!(ctx.task_id.as_deref(), Some("T20260428-7"));
    assert_eq!(ctx.job_run_id.as_deref(), Some("jrun-from-env"));
    assert_eq!(ctx.activity_id.as_deref(), Some("agent_implement"));
    assert_eq!(ctx.step_index, Some(2));
}

#[test]
fn audit_context_treats_run_id_alias_as_job_run_id_input() {
    let _g = env_guard();
    clear_audit_context_env();
    let ctx = resolve_audit_context(
        &json!({ "run_id": "jrun-aliased" }),
        ToolEntryPoint::Cli,
        None,
    );
    assert_eq!(ctx.job_run_id.as_deref(), Some("jrun-aliased"));
}

#[test]
fn standalone_mcp_ignores_tool_and_ambient_identity_claims() {
    let _g = env_guard();
    clear_audit_context_env();
    set_identity_env("codex", "codex");
    set_audit_context_env("env-task", "env-run", "env-activity", "7");
    let context = ToolSessionContext::trusted_local(
        Some("ws_orbit".to_string()),
        Some("hm_local".to_string()),
        Some("local-host".to_string()),
    );
    let audit = resolve_audit_context(
        &json!({
            "task_id": "spoofed-task",
            "job_run_id": "spoofed-run",
            "activity_id": "spoofed-activity",
            "step_index": 99,
            "role": "admin",
            "agent": "claude",
            "model": "claude"
        }),
        ToolEntryPoint::Mcp,
        Some(&context),
    );
    let role = audit_role_label_for_entry_point(
        &json!({"role": "admin", "model": "claude"}),
        Some("claude"),
        Some("claude"),
        ToolEntryPoint::Mcp,
    );
    clear_audit_context_env();
    clear_identity_env();

    assert_eq!(audit.task_id, None);
    assert_eq!(audit.job_run_id, None);
    assert_eq!(audit.activity_id, None);
    assert_eq!(audit.step_index, None);
    assert_eq!(role, "unverified");
}

#[test]
fn managed_mcp_correlation_comes_only_from_the_managed_run_envelope() {
    let _g = env_guard();
    clear_audit_context_env();
    set_identity_env("codex", "codex");
    set_audit_context_env("ORB-10228", "jrun-managed", "agent_implement", "2");
    // SAFETY: tests serialize through `env_guard()` before mutating env.
    unsafe {
        std::env::set_var(ORBIT_MANAGED_RUN_CONTEXT_ENV, "1");
    }
    let audit = trusted_mcp_audit_context();
    let role = audit_role_label_for_entry_point(
        &json!({"model": "claude", "task_id": "spoofed"}),
        None,
        None,
        ToolEntryPoint::Mcp,
    );
    clear_audit_context_env();
    clear_identity_env();

    assert_eq!(audit.task_id.as_deref(), Some("ORB-10228"));
    assert_eq!(audit.job_run_id.as_deref(), Some("jrun-managed"));
    assert_eq!(audit.activity_id.as_deref(), Some("agent_implement"));
    assert_eq!(audit.step_index, Some(2));
    assert_eq!(role, "codex");
}

/// ORB-10727 [ADR-0358]: the run lease is withdrawn, so an unmanaged MCP call
/// correlates to no job run at all. Nothing on the session can supply one.
#[test]
fn unmanaged_mcp_correlates_to_no_job_run() {
    let _g = env_guard();
    clear_audit_context_env();
    let audit = trusted_mcp_audit_context();
    assert_eq!(audit.job_run_id, None);

    // A managed envelope still names the run, and no session field can now
    // contradict it.
    set_audit_context_env("ORB-10228", "jrun-other", "agent_implement", "0");
    // SAFETY: tests serialize through `env_guard()` before mutating env.
    unsafe {
        std::env::set_var(ORBIT_MANAGED_RUN_CONTEXT_ENV, "1");
    }
    let audit = trusted_mcp_audit_context();
    clear_audit_context_env();
    assert_eq!(audit.job_run_id.as_deref(), Some("jrun-other"));
}

#[test]
fn mcp_dispatch_persists_only_trusted_provenance_columns() {
    let _g = env_guard();
    clear_audit_context_env();
    clear_identity_env();
    let runtime = fresh_runtime();
    let mut context = ToolSessionContext::trusted_local(
        Some("ws_orbit".to_string()),
        Some("hm_local".to_string()),
        Some("local-host".to_string()),
    );
    context.origin_session_id = Some("mcp-session-1".to_string());
    context.mcp_call_id = Some("mcall-1".to_string());
    context.trace_id = Some("trace-1".to_string());
    context.caller_ip = Some("192.0.2.10".to_string());

    runtime
        .execute_tool_command_dispatch_with_session_context(
            "orbit.task.list",
            json!({
                "workspace_id": "spoofed-workspace",
                "caller_machine_id": "spoofed-caller",
                "process_machine_id": "spoofed-process",
                "transport": "ssh-mcp",
                "capability": "operator",
                "origin_session_id": "spoofed-session",
                "mcp_call_id": "spoofed-call",
                "lease_id": "spoofed-lease",
                "task_id": "spoofed-task",
                "job_run_id": "spoofed-run",
                "model": "claude"
            }),
            None,
            None,
            ToolEntryPoint::Mcp,
            context,
        )
        .expect("standalone MCP call succeeds");

    let rows = runtime
        .list_audit_events(None, Some("orbit.task.list".to_string()), None, None, 1)
        .expect("read audit row");
    let row = &rows[0];
    assert_eq!(row.role, "unverified");
    assert_eq!(row.workspace_id.as_deref(), Some("ws_orbit"));
    assert_eq!(row.caller_machine_id.as_deref(), Some("hm_local"));
    assert_eq!(row.process_machine_id.as_deref(), Some("hm_local"));
    assert_eq!(row.transport, Some(McpTransport::Local));
    assert_eq!(
        row.effective_capabilities,
        BTreeSet::from([McpCapability::Agent])
    );
    assert_eq!(row.origin_session_id.as_deref(), Some("mcp-session-1"));
    assert_eq!(row.mcp_call_id.as_deref(), Some("mcall-1"));
    assert_eq!(row.trace_id.as_deref(), Some("trace-1"));
    assert_eq!(row.caller_ip.as_deref(), Some("192.0.2.10"));
    assert_eq!(row.task_id, None);
    // Both correlations came from the withdrawn run lease; a spoofed
    // `job_run_id`/`lease_id` in model-authored tool JSON still reaches neither.
    assert_eq!(row.job_run_id, None);
    assert_eq!(row.lease_id, None);
}

#[test]
fn reservation_owner_context_ignores_unmanaged_orbit_run_env() {
    let _g = env_guard();
    clear_audit_context_env();
    // SAFETY: tests serialize through `env_guard()` before mutating env.
    unsafe {
        std::env::set_var("ORBIT_RUN_ID", "jrun-env-owner");
    }

    assert_eq!(reservation_owner_from_env(), None);
    clear_audit_context_env();
}

#[test]
fn reservation_owner_context_comes_from_managed_orbit_run_env() {
    let _g = env_guard();
    clear_audit_context_env();
    // SAFETY: tests serialize through `env_guard()` before mutating env.
    unsafe {
        std::env::set_var("ORBIT_RUN_ID", "jrun-env-owner");
        std::env::set_var(ORBIT_MANAGED_RUN_CONTEXT_ENV, "1");
    }
    let owner = reservation_owner_from_env().expect("owner from managed env");
    clear_audit_context_env();

    assert_eq!(owner.owner_run_id, "jrun-env-owner");
    assert!(
        owner
            .owner_metadata_json
            .as_deref()
            .is_some_and(|raw| { raw.contains("\"source\":\"orbit_cli\"") })
    );
}

#[test]
fn audit_context_returns_none_when_neither_source_supplies_values() {
    let _g = env_guard();
    clear_audit_context_env();
    let ctx = resolve_audit_context(&json!({}), ToolEntryPoint::Cli, None);
    assert!(ctx.task_id.is_none());
    assert!(ctx.job_run_id.is_none());
    assert!(ctx.activity_id.is_none());
    assert!(ctx.step_index.is_none());
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

#[test]
fn dispatch_records_correlation_fields_from_env() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    set_audit_context_env("T20260428-7", "jrun-corr", "agent_implement", "5");

    let outcome = runtime
        .execute_tool_command_dispatch(
            "orbit.search",
            json!({ "query": "anything", "model": orbit_common::test_fixtures::TEST_CODEX_MODEL }),
            None,
            None,
            ToolEntryPoint::Cli,
        )
        .expect("dispatch ok");
    clear_audit_context_env();
    assert!(outcome.audit_recorded);

    let events = runtime
        .list_audit_events(None, Some("orbit.search".to_string()), None, None, 16)
        .expect("list audit events");
    let row = events
        .iter()
        .find(|e| e.execution_id.starts_with("exec-"))
        .expect("at least one row");
    assert_eq!(row.task_id.as_deref(), Some("T20260428-7"));
    assert_eq!(row.job_run_id.as_deref(), Some("jrun-corr"));
    assert_eq!(row.activity_id.as_deref(), Some("agent_implement"));
    assert_eq!(row.step_index, Some(5));
}
