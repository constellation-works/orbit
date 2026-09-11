use chrono::{DateTime, Utc};
use orbit_core::{AuditEvent, AuditEventStatus};
use orbit_types::tool::{McpCapability, McpTransport};
use serde_json::json;

use super::super::support::{AuditListFilters, audit_event_table, audit_event_to_json};

#[test]
fn audit_list_json_projection_shape_is_stable() {
    let event = AuditEvent {
        id: 7,
        execution_id: "exec-1".to_string(),
        timestamp: DateTime::parse_from_rfc3339("2026-05-23T06:00:00Z")
            .expect("timestamp")
            .with_timezone(&Utc),
        command: "tool".to_string(),
        subcommand: Some("orbit.task.show".to_string()),
        tool_name: Some("orbit.task.show".to_string()),
        target_type: Some("task".to_string()),
        target_id: Some("ORB-00276".to_string()),
        role: "codex".to_string(),
        status: AuditEventStatus::Success,
        exit_code: 0,
        duration_ms: 42,
        working_directory: "/workspace".to_string(),
        arguments_json: Some(r#"{"id":"ORB-00276"}"#.to_string()),
        stdout_truncated: Some("{}".to_string()),
        stderr_truncated: None,
        error_message: None,
        host: Some("host.local".to_string()),
        pid: 1234,
        session_id: Some("session-1".to_string()),
        workspace_id: Some("ws-orbit".to_string()),
        caller_machine_id: Some("hm-caller".to_string()),
        caller_host_id: Some("caller.local".to_string()),
        process_machine_id: Some("hm-process".to_string()),
        process_host_id: Some("process.local".to_string()),
        transport: Some(McpTransport::Local),
        trace_id: Some("trace-1".to_string()),
        caller_ip: Some("192.0.2.8".to_string()),
        effective_capabilities: [McpCapability::Agent].into_iter().collect(),
        origin_session_id: Some("origin-session-1".to_string()),
        mcp_call_id: Some("mcall-1".to_string()),
        lease_id: Some("lease-1".to_string()),
        task_id: Some("ORB-00276".to_string()),
        job_run_id: Some("jrun-1".to_string()),
        activity_id: Some("implement".to_string()),
        step_index: Some(2),
        self_reported_actor: Some("claude-code".to_string()),
    };

    assert_eq!(
        audit_event_to_json(&event),
        json!({
            "id": 7,
            "execution_id": "exec-1",
            "timestamp": "2026-05-23T06:00:00+00:00",
            "command": "tool",
            "subcommand": "orbit.task.show",
            "tool_name": "orbit.task.show",
            "target_type": "task",
            "target_id": "ORB-00276",
            "role": "codex",
            "status": "success",
            "exit_code": 0,
            "duration_ms": 42,
            "working_directory": "/workspace",
            "arguments_json": "{\"id\":\"ORB-00276\"}",
            "stdout_truncated": "{}",
            "stderr_truncated": null,
            "error_message": null,
            "host": "host.local",
            "pid": 1234,
            "session_id": "session-1",
            "workspace_id": "ws-orbit",
            "caller_machine_id": "hm-caller",
            "caller_host_id": "caller.local",
            "process_machine_id": "hm-process",
            "process_host_id": "process.local",
            "transport": "local",
            "trace_id": "trace-1",
            "caller_ip": "192.0.2.8",
            "effective_capabilities": ["agent"],
            "origin_session_id": "origin-session-1",
            "mcp_call_id": "mcall-1",
            "lease_id": "lease-1",
            "task_id": "ORB-00276",
            "job_run_id": "jrun-1",
            "activity_id": "implement",
            "step_index": 2,
            // ORB-10890: carried under its own key, next to but never merged
            // with the authenticated `role` above.
            "self_reported_actor": "claude-code",
        })
    );
}

/// One denied row, with every column a run of denied rows tends to agree on:
/// the same second, the same role, and no tool name.
fn denied_event(id: i64, execution_id: &str) -> AuditEvent {
    AuditEvent {
        id,
        execution_id: execution_id.to_string(),
        timestamp: DateTime::parse_from_rfc3339("2026-09-10T04:15:00Z")
            .expect("timestamp")
            .with_timezone(&Utc),
        command: "task".to_string(),
        subcommand: None,
        tool_name: None,
        target_type: Some("task".to_string()),
        target_id: None,
        role: "claude".to_string(),
        status: AuditEventStatus::Denied,
        exit_code: 1,
        duration_ms: 3,
        working_directory: "/workspace".to_string(),
        arguments_json: None,
        stdout_truncated: None,
        stderr_truncated: None,
        error_message: Some("policy denied".to_string()),
        host: None,
        pid: 1234,
        session_id: None,
        workspace_id: None,
        caller_machine_id: None,
        caller_host_id: None,
        process_machine_id: None,
        process_host_id: None,
        transport: None,
        trace_id: None,
        caller_ip: None,
        effective_capabilities: Default::default(),
        origin_session_id: None,
        mcp_call_id: None,
        lease_id: None,
        task_id: None,
        job_run_id: None,
        activity_id: None,
        step_index: None,
        self_reported_actor: None,
    }
}

/// [ORB-12113] `orbit audit list --status denied` emitted `denied<TAB>task<TAB>3`
/// while a mixed-status listing emitted all six fields: the denied rows agreed
/// on time, role and tool, and uniform-column suppression dropped those three
/// from a form that has no header to show they had gone. Every row of the
/// piped form now carries every column, whatever the values are.
#[test]
fn piped_denied_rows_carry_the_same_columns_as_every_other_status() {
    use crate::output::sink::{OutputSink, SinkEnv};

    let events = vec![denied_event(1, "exec-a"), denied_event(2, "exec-b")];
    let table = audit_event_table(
        &events,
        AuditListFilters {
            status: true,
            ..AuditListFilters::default()
        },
    );

    let piped = OutputSink::resolve(false, &SinkEnv::default(), None, None, false);
    let rendered = table.render_plain(&piped);
    assert_eq!(rendered.lines().count(), events.len());
    for line in rendered.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        assert_eq!(fields.len(), 6, "dropped a column from `{line}`");
        assert_eq!(fields[0], "2026-09-10T04:15:00");
        assert_eq!(fields[1], "denied");
        assert_eq!(fields[2], "claude");
        assert_eq!(fields[3], "task");
        assert_eq!(fields[4], "-");
        assert_eq!(fields[5], "3");
    }
}
