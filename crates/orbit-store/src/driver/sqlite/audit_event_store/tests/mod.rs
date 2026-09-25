mod aggregates;
mod audit_actor;
mod incident;
mod insert;
mod queries;
mod self_reported_actor;

use crate::contracts::AuditEventInsertParams;
use orbit_common::test_fixtures::TEST_CLAUDE_MODEL;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::McpCapability;
use orbit_types::tool::McpTransport;
use std::collections::BTreeSet;

fn sample_params() -> AuditEventInsertParams {
    AuditEventInsertParams {
        execution_id: "exec-test-1".to_string(),
        command: "tool".to_string(),
        subcommand: Some("run".to_string()),
        tool_name: Some("orbit.task.show".to_string()),
        target_type: Some("tool".to_string()),
        target_id: Some("orbit.task.show".to_string()),
        role: TEST_CLAUDE_MODEL.to_string(),
        status: AuditEventStatus::Success,
        exit_code: 0,
        duration_ms: 42,
        working_directory: "/tmp".to_string(),
        arguments_json: None,
        stdout_truncated: None,
        stderr_truncated: None,
        error_message: None,
        host: Some("test-host".to_string()),
        pid: 1234,
        session_id: Some("session-abc".to_string()),
        workspace_id: Some("ws_orbit".to_string()),
        caller_machine_id: Some("hm_caller".to_string()),
        caller_machine_name: Some("caller-host".to_string()),
        process_machine_id: Some("hm_process".to_string()),
        process_machine_name: Some("process-host".to_string()),
        transport: Some(McpTransport::SshMcp),
        effective_capabilities: BTreeSet::from([McpCapability::Agent, McpCapability::Runner]),
        origin_session_id: Some("mcp-session-abc".to_string()),
        mcp_call_id: Some("mcall-abc".to_string()),
        lease_id: Some("lease-abc".to_string()),
        task_id: Some("T20260428-7".to_string()),
        job_run_id: Some("jrun-xyz".to_string()),
        activity_id: Some("agent_implement".to_string()),
        step_index: Some(2),
    }
}

fn sample_params_with(
    execution_id: &str,
    role: &str,
    status: AuditEventStatus,
) -> AuditEventInsertParams {
    AuditEventInsertParams {
        execution_id: execution_id.to_string(),
        role: role.to_string(),
        status,
        ..sample_params()
    }
}
