use orbit_types::telemetry::AuditEventStatus;

use crate::{AuditEventInsertParams, OrbitRuntime};

fn event(index: usize) -> AuditEventInsertParams {
    AuditEventInsertParams {
        execution_id: format!("exec-{index}"),
        command: "test".to_string(),
        subcommand: None,
        tool_name: Some("fs.read".to_string()),
        target_type: None,
        target_id: None,
        role: "operator".to_string(),
        status: AuditEventStatus::Success,
        exit_code: 0,
        duration_ms: 0,
        working_directory: "/".to_string(),
        arguments_json: None,
        stdout_truncated: None,
        stderr_truncated: None,
        error_message: None,
        host: None,
        pid: 1,
        session_id: None,
        workspace_id: None,
        caller_machine_id: None,
        caller_machine_name: None,
        process_machine_id: None,
        process_machine_name: None,
        transport: None,
        effective_capabilities: Default::default(),
        origin_session_id: None,
        mcp_call_id: None,
        lease_id: None,
        task_id: None,
        job_run_id: None,
        activity_id: None,
        step_index: None,
    }
}

/// An export is every matching event, not the store's default 1000-row page.
#[test]
fn export_audit_events_reads_past_one_page() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    for index in 0..1005 {
        runtime.record_audit_event(&event(index)).expect("record");
    }

    let events = runtime.export_audit_events(None, None).expect("export");

    assert_eq!(events.len(), 1005);
    assert!(events.windows(2).all(|pair| pair[0].id > pair[1].id));
}
