//! Shared behavioral fixture for the real dashboard and CLI audit surfaces.

use orbit_core::{
    AuditEventInsertParams, AuditEventStatus, OrbitRuntime, V2AuditEventInsertParams,
};
use serde_json::json;

pub const SQL_POLICY_COUNT: i64 = 7;
pub const V2_POLICY_COUNT: i64 = 1;
pub const RAW_DENIED_COUNT: i64 = 12;

pub fn seed(runtime: &OrbitRuntime) {
    let capability = "operation 'orbit.workflow.run.show' requires the `operator` capability (inspect runs); this caller was resolved as `agent` holding [agent].";
    let rows = [
        (
            "lock",
            "task.locks.reserve.denied",
            "orbit.task.locks.reserve",
            "held lock",
        ),
        (
            "protocol",
            "tool",
            "orbit.drain.claim.settle",
            "this owner declares no required validation commands (`workflow.required_validation_commands`), so no handoff can be accepted",
        ),
        (
            "auth-legacy",
            "authorization",
            "orbit.workflow.run.show",
            capability,
        ),
        ("tool-legacy", "tool", "orbit.workflow.run.show", capability),
        (
            "auth-call-1",
            "authorization",
            "orbit.command.exec",
            "denied call",
        ),
        ("tool-call-1", "tool", "orbit.command.exec", "denied call"),
        (
            "auth-call-2",
            "authorization",
            "orbit.command.exec",
            "denied call",
        ),
        ("tool-call-2", "tool", "orbit.command.exec", "denied call"),
        (
            "spawn",
            "tool",
            "proc.spawn",
            "executable denied by allowlist",
        ),
        (
            "fs",
            "tool",
            "fs.read",
            "fs.read denied for `/private` under fsProfile `workspace`",
        ),
        (
            "auth-lock",
            "authorization",
            "orbit.task.locks.reserve",
            "capability refusal",
        ),
        (
            "auth-settle",
            "authorization",
            "orbit.drain.claim.settle",
            "capability refusal",
        ),
    ];
    for (id, command, operation, message) in rows {
        let is_authorization = command == "authorization";
        let session = if id.ends_with("call-1") {
            Some("session-one".to_string())
        } else if id.ends_with("call-2") {
            Some("session-two".to_string())
        } else {
            None
        };
        runtime
            .record_audit_event(&AuditEventInsertParams {
                execution_id: id.to_string(),
                command: command.to_string(),
                subcommand: Some("run-mcp".to_string()),
                tool_name: (!is_authorization).then(|| operation.to_string()),
                target_type: Some(
                    if is_authorization {
                        "operation"
                    } else {
                        "tool"
                    }
                    .to_string(),
                ),
                target_id: Some(if id == "fs" { "/private" } else { operation }.to_string()),
                role: "codex".to_string(),
                status: AuditEventStatus::Denied,
                exit_code: 1,
                duration_ms: 1,
                working_directory: ".".to_string(),
                arguments_json: (id == "lock").then(|| {
                    json!({
                        "files": ["src/shared.rs"], "conflicts": [{"held_by": "another worker"}]
                    })
                    .to_string()
                }),
                stdout_truncated: None,
                stderr_truncated: None,
                error_message: Some(message.to_string()),
                host: None,
                pid: 1,
                session_id: session,
                workspace_id: None,
                caller_machine_id: Some("fixture-machine".to_string()),
                caller_machine_name: None,
                process_machine_id: None,
                process_machine_name: None,
                transport: None,
                effective_capabilities: Default::default(),
                origin_session_id: None,
                // Reused request IDs in distinct sessions must remain two attempts.
                mcp_call_id: id.contains("call-").then(|| "request-1".to_string()),
                lease_id: None,
                task_id: None,
                job_run_id: None,
                activity_id: None,
                step_index: None,
            })
            .unwrap();
    }
    runtime
        .insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: runtime.workspace_id().unwrap(),
            event_id: "policy-fixture-v2".to_string(),
            source: "loop".to_string(),
            schema_version: 1,
            event_type: "tool.denied".to_string(),
            ts: chrono::Utc::now(),
            run_id: "policy-fixture-run".to_string(),
            agent_identity: "codex".to_string(),
            parent_event_id: None,
            workspace_path: None,
            payload_json: json!({"tool_name": "fixture.denied", "reason": "tool allowlist"})
                .to_string(),
        })
        .unwrap();
}
