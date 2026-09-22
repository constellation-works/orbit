use orbit_common::OrbitError;
use orbit_store::Store;
use orbit_tools::ToolExecutionKind;
use orbit_tools::plugin::PluginCallbackSession;
use orbit_types::plugin::{InstalledPlugin, PluginProvenance};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::{McpCapability, McpTransport, ToolSessionContext};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;

use serde_json::json;

use super::support::{clear_identity_env, env_guard, fresh_runtime, set_identity_env};
use crate::OrbitRuntime;
use crate::adapter::command::dispatch::{
    ORBIT_MANAGED_RUN_CONTEXT_ENV, ORBIT_PLUGIN_ENV, ToolEntryPoint, audit_role_label,
    audit_role_label_for_entry_point, finalize_successful_dispatch,
    override_activity_tools_for_test, reservation_owner_from_env, resolve_audit_context,
    take_tool_audit_recorded, trusted_mcp_audit_context,
};
use crate::runtime::plugin_host::plugin_install_path;

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

fn record_callback_plugin(runtime: &OrbitRuntime, orbit_tools: &[&str]) {
    let root = plugin_install_path(&runtime.global_root(), "callback", "1.0.0");
    record_callback_plugin_tree(&root, orbit_tools);
    runtime
        .stores()
        .plugins()
        .upsert_plugin(&InstalledPlugin {
            name: "callback".to_string(),
            version: "1.0.0".to_string(),
            source: "fixture".to_string(),
            install_path: root.to_string_lossy().into_owned(),
            archive_digest: None,
            manifest_digest: "0".repeat(64),
            enabled: true,
            grants: vec!["orbit_tools".to_string()],
            first_party: false,
            certified_orbit_version: None,
            installed_at: String::new(),
            updated_at: String::new(),
        })
        .expect("record the install");
}

/// The plugin tree alone, so a test can write a second one somewhere the row
/// has no business pointing at.
fn record_callback_plugin_tree(root: &Path, orbit_tools: &[&str]) {
    let name = "callback";
    let version = "1.0.0";
    std::fs::create_dir_all(root.join("bin")).expect("create plugin bin");
    let backend = root.join("bin/backend.sh");
    std::fs::write(
        &backend,
        "#!/bin/sh\ncat >/dev/null\necho '{\"ok\":true,\"output\":{}}'\n",
    )
    .expect("write backend");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o755))
            .expect("chmod backend");
    }
    let requested = orbit_tools.join(", ");
    std::fs::write(
        root.join("plugin.yaml"),
        format!(
            "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: {name}\n  version: {version}\nspec:\n  permissions:\n    orbit_tools: [{requested}]\n  backend:\n    type: exec\n    command: bin/backend.sh\n  tools:\n    - name: hello\n      execution_kind: read_only\n      mcp_scope: workspace\n"
        ),
    )
    .expect("write manifest");
}

fn set_plugin_callback_env(plugin: &str, allowed_tools: Option<&str>) {
    // SAFETY: callers hold `env_guard()` while changing process environment.
    unsafe {
        std::env::set_var(ORBIT_PLUGIN_ENV, plugin);
        match allowed_tools {
            Some(value) => std::env::set_var("ORBIT_ALLOWED_TOOLS", value),
            None => std::env::remove_var("ORBIT_ALLOWED_TOOLS"),
        }
    }
}

/// A live session whose ceiling is everything the recorded install requests:
/// the spawning caller was unrestricted, which is the shape every test that
/// predates the ceiling assumes.
fn bind_live_callback_session(runtime: &OrbitRuntime) -> PluginCallbackSession {
    let requested = recorded_orbit_tools_request(runtime);
    bind_live_callback_session_with_ceiling(runtime, &requested)
}

/// The same session minted for a caller whose own allowlist was narrower than
/// the plugin's manifest request [ORB-12801].
fn bind_live_callback_session_with_ceiling(
    runtime: &OrbitRuntime,
    effective_tools: &[String],
) -> PluginCallbackSession {
    let installed = runtime
        .stores()
        .plugins()
        .get_plugin("callback")
        .expect("read plugin")
        .expect("callback plugin is recorded");
    let mut session = PluginCallbackSession::mint(
        &runtime.global_root(),
        &PluginProvenance {
            name: installed.name,
            version: installed.version,
            manifest_digest: installed.manifest_digest,
            grants: installed.grants,
        },
        effective_tools,
    )
    .expect("mint callback session");
    session
        .bind_pid(std::process::id())
        .expect("bind this process as the plugin child");
    session
}

/// Hold a live session's record open on the descriptor a spawned backend
/// inherits, and name that descriptor to the resolver.
///
/// This is the credential in production: the host opens the record and maps it
/// onto file descriptor 3 in the child. Tests cannot dictate a process-wide
/// descriptor number, so they name the one they got — the same seam the
/// environment variable exists for. Dropping the returned file is what a
/// descendant that sheds the credential does.
fn present_callback_descriptor(session: &PluginCallbackSession) -> std::fs::File {
    use std::os::fd::AsRawFd;

    let file = std::fs::File::open(session.path()).expect("open the session record");
    // SAFETY: callers hold `env_guard()` while changing process environment.
    unsafe {
        std::env::set_var(
            orbit_tools::plugin::ORBIT_PLUGIN_CALLBACK_FD_ENV,
            file.as_raw_fd().to_string(),
        );
    }
    file
}

/// Turn the deprecation on for this host, so the retired environment token and
/// process ancestry identify a callback for one more release [ORB-12841].
fn enable_legacy_callback_identity(runtime: &OrbitRuntime) {
    let path = runtime.global_root().join("config.toml");
    let mut document = std::fs::read_to_string(&path).unwrap_or_default();
    document.push_str("\n[plugin]\nlegacy_callback_identity = true\n");
    std::fs::write(&path, document).expect("write the host config");
}

/// What the recorded manifest asks for under `permissions.orbit_tools`.
fn recorded_orbit_tools_request(runtime: &OrbitRuntime) -> Vec<String> {
    let installed = runtime
        .stores()
        .plugins()
        .get_plugin("callback")
        .expect("read plugin")
        .expect("callback plugin is recorded");
    orbit_tools::plugin::load_plugin_dir(std::path::Path::new(&installed.install_path))
        .expect("load the recorded install")
        .manifest
        .spec
        .permissions
        .orbit_tools
}

fn dispatch_entry(
    runtime: &OrbitRuntime,
    tool: &str,
    entry_point: ToolEntryPoint,
) -> Result<serde_json::Value, OrbitError> {
    let input = match tool {
        "orbit.search" => json!({
            "query": "callback",
            "model": orbit_common::test_fixtures::TEST_CODEX_MODEL
        }),
        "orbit.task.list" => json!({ "limit": 10 }),
        other => panic!("dispatch fixture does not cover {other}"),
    };
    runtime
        .execute_tool_command_dispatch(tool, input, None, None, entry_point)
        .map(|outcome| outcome.value)
}

fn dispatch_cli(runtime: &OrbitRuntime, tool: &str) -> Result<serde_json::Value, OrbitError> {
    dispatch_entry(runtime, tool, ToolEntryPoint::Cli)
}

fn assert_plugin_allowlist_denied(error: &OrbitError, tool: &str) {
    let message = error.to_string();
    assert!(
        matches!(error, OrbitError::PolicyDenied(_)),
        "expected PolicyDenied, got {error}"
    );
    assert!(
        message.contains(tool) && message.contains("granted orbit_tools allowlist"),
        "{message}"
    );
}

/// A plugin callback that unsets or rewrites `ORBIT_ALLOWED_TOOLS` is still
/// bounded by the recorded install, not by the inherited variable.
#[test]
fn plugin_callback_allowlist_ignores_forged_or_unset_env() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    record_callback_plugin(&runtime, &["orbit.task.list"]);
    let session = bind_live_callback_session(&runtime);
    let _credential = present_callback_descriptor(&session);

    let run = |allowed_tools: Option<&str>, tool: &str| {
        set_plugin_callback_env("callback", allowed_tools);
        dispatch_cli(&runtime, tool)
    };

    run(None, "orbit.task.list").expect("unset env still admits a recorded tool");
    run(Some("orbit.search,orbit.task.add"), "orbit.task.list")
        .expect("rewritten env still admits a recorded tool");

    assert_plugin_allowlist_denied(
        &run(None, "orbit.search").expect_err("unset env must not admit an unrecorded tool"),
        "orbit.search",
    );
    assert_plugin_allowlist_denied(
        &run(Some("orbit.search"), "orbit.search")
            .expect_err("rewritten env must not admit an unrecorded tool"),
        "orbit.search",
    );
}

/// A backend already running cannot widen its own allowlist by relocating its
/// row: the callback gate reads `permissions.orbit_tools` out of the tree the
/// row names, so the path is held to the install root on every call, not only
/// at load [ORB-12785].
#[test]
fn plugin_callback_allowlist_refuses_a_row_repointed_outside_the_install_root() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    record_callback_plugin(&runtime, &["orbit.task.list"]);
    let session = bind_live_callback_session(&runtime);
    let _credential = present_callback_descriptor(&session);
    set_plugin_callback_env("callback", None);
    dispatch_cli(&runtime, "orbit.task.list").expect("the recorded install admits its own tool");

    // The attacker's tree, under a directory `orbit_tools` lets the backend
    // write, with a manifest that asks for everything.
    let global_root = runtime.global_root();
    let evil = global_root.join("state/logs/evil");
    record_callback_plugin_tree(&evil, &["orbit.task.list", "orbit.search"]);
    let mut installed = runtime
        .stores()
        .plugins()
        .get_plugin("callback")
        .expect("read plugin")
        .expect("recorded");
    installed.install_path = evil.to_string_lossy().into_owned();
    runtime
        .stores()
        .plugins()
        .upsert_plugin(&installed)
        .expect("the attacker's row write succeeds; the gate is what refuses it");

    for tool in ["orbit.search", "orbit.task.list"] {
        let error = dispatch_cli(&runtime, tool)
            .expect_err("no allowlist is read out of a tree outside the install root");
        let message = error.to_string();
        assert!(
            matches!(error, OrbitError::PolicyDenied(_))
                && message.contains(&installed.install_path)
                && message.contains(&global_root.join("plugins/callback").display().to_string()),
            "{tool}: {message}"
        );
    }
}

#[test]
fn plugin_callback_allowlist_is_idle_without_orbit_plugin() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    record_callback_plugin(&runtime, &["orbit.task.list"]);
    // SAFETY: callers hold `env_guard()` while changing process environment.
    unsafe {
        std::env::remove_var(ORBIT_PLUGIN_ENV);
        std::env::set_var("ORBIT_ALLOWED_TOOLS", "orbit.task.list");
    }
    dispatch_cli(&runtime, "orbit.search")
        .expect("ordinary CLI callers are not gated by a plugin allowlist");
}

/// Clearing `ORBIT_PLUGIN` in a live backend process does not drop the
/// allowlist: ancestry still names the plugin.
#[test]
fn plugin_callback_allowlist_holds_after_orbit_plugin_is_cleared() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    record_callback_plugin(&runtime, &["orbit.task.list"]);
    let session = bind_live_callback_session(&runtime);
    let _credential = present_callback_descriptor(&session);
    // SAFETY: callers hold `env_guard()` while changing process environment.
    unsafe {
        std::env::remove_var(ORBIT_PLUGIN_ENV);
        std::env::remove_var(orbit_tools::plugin::ORBIT_PLUGIN_CALLBACK_ENV);
        std::env::remove_var("ORBIT_ALLOWED_TOOLS");
    }

    dispatch_cli(&runtime, "orbit.task.list")
        .expect("a recorded callback still runs after ORBIT_PLUGIN is cleared");
    assert_plugin_allowlist_denied(
        &dispatch_cli(&runtime, "orbit.search")
            .expect_err("clearing ORBIT_PLUGIN must not admit an unrecorded tool"),
        "orbit.search",
    );
}

#[test]
fn plugin_callback_allowlist_applies_on_mcp_entry_point() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    record_callback_plugin(&runtime, &["orbit.task.list"]);
    let session = bind_live_callback_session(&runtime);
    let _credential = present_callback_descriptor(&session);
    // SAFETY: callers hold `env_guard()` while changing process environment.
    unsafe {
        std::env::remove_var(ORBIT_PLUGIN_ENV);
        std::env::remove_var(orbit_tools::plugin::ORBIT_PLUGIN_CALLBACK_ENV);
    }

    dispatch_entry(&runtime, "orbit.task.list", ToolEntryPoint::Mcp)
        .expect("a recorded callback still runs over MCP");
    assert_plugin_allowlist_denied(
        &dispatch_entry(&runtime, "orbit.search", ToolEntryPoint::Mcp)
            .expect_err("MCP must apply the same plugin allowlist as CLI"),
        "orbit.search",
    );
}

#[test]
fn plugin_callback_refusal_is_audited_with_plugin_identity() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    record_callback_plugin(&runtime, &["orbit.task.list"]);
    let session = bind_live_callback_session(&runtime);
    let _credential = present_callback_descriptor(&session);
    // SAFETY: callers hold `env_guard()` while changing process environment.
    unsafe {
        std::env::remove_var(ORBIT_PLUGIN_ENV);
        std::env::remove_var(orbit_tools::plugin::ORBIT_PLUGIN_CALLBACK_ENV);
    }

    assert_plugin_allowlist_denied(
        &dispatch_cli(&runtime, "orbit.search").expect_err("unrecorded tool is refused"),
        "orbit.search",
    );

    let events = runtime
        .list_audit_events(None, Some("orbit.search".to_string()), None, None, 16)
        .expect("list audit events");
    let row = events
        .iter()
        .find(|event| event.status == AuditEventStatus::Denied)
        .expect("denied callback row");
    let plugin = row.plugin.as_ref().expect("plugin identity on the refusal");
    assert_eq!(plugin.name, "callback");
    assert_eq!(plugin.version, "1.0.0");
}

/// A backend descendant that changed process group and dropped the credential
/// is not an ordinary caller. The plugin sandbox is what it cannot shed: the
/// host-owned session directory stays unreadable to it, and an unreadable
/// session directory with no credential is a refusal on both entry points
/// [ORB-12798].
#[cfg(unix)]
#[test]
fn plugin_callback_refuses_an_unidentified_confined_child() {
    use std::os::unix::fs::PermissionsExt;

    let _g = env_guard();
    let runtime = fresh_runtime();
    record_callback_plugin(&runtime, &["orbit.task.list"]);
    // SAFETY: callers hold `env_guard()` while changing process environment.
    unsafe {
        std::env::remove_var(ORBIT_PLUGIN_ENV);
        std::env::remove_var(orbit_tools::plugin::ORBIT_PLUGIN_CALLBACK_ENV);
    }

    let sessions = runtime.global_root().join("state/plugin-callbacks");
    std::fs::create_dir_all(&sessions).expect("create the session directory");
    std::fs::set_permissions(&sessions, std::fs::Permissions::from_mode(0o000))
        .expect("make the session directory unreadable");
    if std::fs::read_dir(&sessions).is_ok() {
        // Root ignores directory permissions; the kernel-enforced case is the
        // CLI regression through the real plugin sandbox.
        std::fs::set_permissions(&sessions, std::fs::Permissions::from_mode(0o700))
            .expect("restore");
        return;
    }

    let refusals: Vec<OrbitError> = [ToolEntryPoint::Cli, ToolEntryPoint::Mcp]
        .into_iter()
        .map(|entry_point| {
            dispatch_entry(&runtime, "orbit.task.list", entry_point)
                .expect_err("an unidentified plugin child is refused")
        })
        .collect();
    std::fs::set_permissions(&sessions, std::fs::Permissions::from_mode(0o700)).expect("restore");

    for error in refusals {
        assert!(matches!(error, OrbitError::PolicyDenied(_)), "{error}");
        assert!(
            error
                .to_string()
                .contains("without the host-issued callback session"),
            "{error}"
        );
    }
}

/// A live token identifies the process the host bound it to. Presenting one
/// that belongs to another process — read out of the session directory, or
/// kept across a `setsid` — is a mismatch, never that plugin's allowlist.
#[test]
fn plugin_callback_refuses_a_token_bound_to_another_process() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    record_callback_plugin(&runtime, &["orbit.task.list"]);
    enable_legacy_callback_identity(&runtime);
    let installed = runtime
        .stores()
        .plugins()
        .get_plugin("callback")
        .expect("read plugin")
        .expect("callback plugin is recorded");
    let mut session = PluginCallbackSession::mint(
        &runtime.global_root(),
        &PluginProvenance {
            name: installed.name,
            version: installed.version,
            manifest_digest: installed.manifest_digest,
            grants: installed.grants,
        },
        &["orbit.task.list".to_string()],
    )
    .expect("mint callback session");
    // pid 1 is live and is never this process, its parent, or its group.
    session.bind_pid(1).expect("bind another process");
    // SAFETY: callers hold `env_guard()` while changing process environment.
    unsafe {
        std::env::set_var(
            orbit_tools::plugin::ORBIT_PLUGIN_CALLBACK_ENV,
            session.token(),
        );
    }

    let error = dispatch_cli(&runtime, "orbit.task.list")
        .expect_err("a token bound to another process is not this caller's credential");
    // SAFETY: callers hold `env_guard()` while changing process environment.
    unsafe {
        std::env::remove_var(orbit_tools::plugin::ORBIT_PLUGIN_CALLBACK_ENV);
    }
    assert!(matches!(error, OrbitError::PolicyDenied(_)), "{error}");
    assert!(
        error
            .to_string()
            .contains("not held by the calling process"),
        "{error}"
    );
}

/// Clear every restriction the child controls: the namespace, the
/// informational allowlist, the callback token, and the activity envelope
/// that would otherwise bound `ToolContext.allowed_tools`. What remains is
/// the host-owned session, which is the whole point [ORB-12801].
fn shed_child_restrictions() {
    // SAFETY: callers hold `env_guard()` while changing process environment.
    unsafe {
        std::env::remove_var(ORBIT_PLUGIN_ENV);
        std::env::remove_var(orbit_tools::plugin::ORBIT_PLUGIN_CALLBACK_ENV);
        std::env::remove_var("ORBIT_ALLOWED_TOOLS");
        std::env::remove_var("ORBIT_ACTIVITY_TOOLS");
        std::env::remove_var("ORBIT_TASK_ACTOR_KIND");
    }
}

fn assert_ceiling_denied(error: &OrbitError, tool: &str) {
    assert_plugin_allowlist_denied(error, tool);
    assert!(
        error.to_string().contains("never widens"),
        "the refusal must name the session ceiling, not only the manifest: {error}"
    );
}

/// The manifest requests two tools and the host granted `orbit_tools`, but
/// the caller that spawned this backend could reach only one of them. The
/// second is refused on both entry points even though the child has shed
/// every restriction it carries in its own environment.
#[test]
fn plugin_callback_cannot_exceed_the_spawning_callers_ceiling() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    record_callback_plugin(&runtime, &["orbit.task.list", "orbit.search"]);
    let session =
        bind_live_callback_session_with_ceiling(&runtime, &["orbit.task.list".to_string()]);
    let _credential = present_callback_descriptor(&session);
    shed_child_restrictions();

    for entry_point in [ToolEntryPoint::Cli, ToolEntryPoint::Mcp] {
        dispatch_entry(&runtime, "orbit.task.list", entry_point)
            .expect("the tool inside the caller's ceiling still runs");
        assert_ceiling_denied(
            &dispatch_entry(&runtime, "orbit.search", entry_point).expect_err(
                "a manifest-listed tool outside the caller's ceiling must not be dispatched",
            ),
            "orbit.search",
        );
    }
}

/// Two live sessions of the same plugin, minted for callers with different
/// allowlists. Each token carries its own ceiling; neither borrows the
/// other's.
#[test]
fn separate_callback_sessions_keep_their_own_ceilings() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    record_callback_plugin(&runtime, &["orbit.task.list", "orbit.search"]);
    let lister =
        bind_live_callback_session_with_ceiling(&runtime, &["orbit.task.list".to_string()]);
    let searcher = bind_live_callback_session_with_ceiling(&runtime, &["orbit.search".to_string()]);
    shed_child_restrictions();

    let as_session = |session: &PluginCallbackSession, tool: &str| {
        let _credential = present_callback_descriptor(session);
        dispatch_cli(&runtime, tool)
    };

    as_session(&lister, "orbit.task.list").expect("the lister's own tool");
    assert_ceiling_denied(
        &as_session(&lister, "orbit.search")
            .expect_err("the lister must not reach the searcher's tool"),
        "orbit.search",
    );
    as_session(&searcher, "orbit.search").expect("the searcher's own tool");
    assert_ceiling_denied(
        &as_session(&searcher, "orbit.task.list")
            .expect_err("the searcher must not reach the lister's tool"),
        "orbit.task.list",
    );
}

/// A backend can write its own install tree, so it can widen what the row's
/// manifest requests while it is still running. The recorded list is re-read
/// on every callback — and intersected with a ceiling that was fixed when the
/// host minted the session, so the widened request buys nothing.
#[test]
fn a_live_callback_session_does_not_widen_when_the_recorded_manifest_does() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    record_callback_plugin(&runtime, &["orbit.task.list"]);
    let session =
        bind_live_callback_session_with_ceiling(&runtime, &["orbit.task.list".to_string()]);
    let _credential = present_callback_descriptor(&session);
    shed_child_restrictions();
    dispatch_cli(&runtime, "orbit.task.list").expect("the recorded install admits its own tool");

    // Rewrite the manifest in place, inside the install root the gate holds
    // the row to, so nothing but the requested allowlist changes.
    record_callback_plugin_tree(
        &plugin_install_path(&runtime.global_root(), "callback", "1.0.0"),
        &["orbit.task.list", "orbit.search"],
    );

    assert_ceiling_denied(
        &dispatch_cli(&runtime, "orbit.search")
            .expect_err("widening the recorded request must not widen a live session"),
        "orbit.search",
    );
    dispatch_cli(&runtime, "orbit.task.list")
        .expect("the session's own tool is unaffected by the rewrite");
}

/// The other direction, which the ceiling must not freeze: revocation. The
/// recorded grant is read on every callback, so withdrawing `orbit_tools`
/// stops a session that is already live at its next call — there is no
/// session to kill and no cache to invalidate.
#[test]
fn revoking_the_grant_stops_a_live_callback_session() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    record_callback_plugin(&runtime, &["orbit.task.list"]);
    let session =
        bind_live_callback_session_with_ceiling(&runtime, &["orbit.task.list".to_string()]);
    let _credential = present_callback_descriptor(&session);
    shed_child_restrictions();
    dispatch_cli(&runtime, "orbit.task.list").expect("the granted callback runs");

    let mut installed = runtime
        .stores()
        .plugins()
        .get_plugin("callback")
        .expect("read plugin")
        .expect("recorded");
    installed.grants = Vec::new();
    runtime
        .stores()
        .plugins()
        .upsert_plugin(&installed)
        .expect("revoke orbit_tools");

    assert_plugin_allowlist_denied(
        &dispatch_cli(&runtime, "orbit.task.list")
            .expect_err("a revoked grant refuses the next callback of a live session"),
        "orbit.task.list",
    );
}
