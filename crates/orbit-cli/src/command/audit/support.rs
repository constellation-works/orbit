use orbit_core::AuditEvent;
use serde_json::{Value, json};

use crate::output::color::{Domain, cell};
use crate::output::table::{Column, Table};

pub(super) fn audit_event_to_json(event: &AuditEvent) -> Value {
    json!({
        "id": event.id,
        "execution_id": event.execution_id,
        "timestamp": event.timestamp.to_rfc3339(),
        "command": event.command,
        "subcommand": event.subcommand,
        "tool_name": event.tool_name,
        "target_type": event.target_type,
        "target_id": event.target_id,
        "role": event.role,
        "status": event.status.to_string(),
        "exit_code": event.exit_code,
        "duration_ms": event.duration_ms,
        "working_directory": event.working_directory,
        "arguments_json": event.arguments_json,
        "stdout_truncated": event.stdout_truncated,
        "stderr_truncated": event.stderr_truncated,
        "error_message": event.error_message,
        "host": event.host,
        "pid": event.pid,
        "session_id": event.session_id,
        "workspace_id": event.workspace_id,
        "caller_machine_id": event.caller_machine_id,
        "caller_machine_name": event.caller_machine_name,
        "process_machine_id": event.process_machine_id,
        "process_machine_name": event.process_machine_name,
        "transport": event.transport,
        "trace_id": event.trace_id,
        "caller_ip": event.caller_ip,
        "effective_capabilities": event.effective_capabilities,
        "origin_session_id": event.origin_session_id,
        "mcp_call_id": event.mcp_call_id,
        "lease_id": event.lease_id,
        "task_id": event.task_id,
        "job_run_id": event.job_run_id,
        "activity_id": event.activity_id,
        "step_index": event.step_index,
        // ORB-10890: named `self_reported_*` rather than `actor` precisely so
        // a consumer cannot mistake it for the authenticated `role` above.
        "self_reported_actor": event.self_reported_actor,
    })
}

/// Which columns the caller filtered on, so a column the filter made
/// informationally uniform still stays on screen.
#[derive(Clone, Copy, Default)]
pub(super) struct AuditListFilters {
    pub(super) status: bool,
    pub(super) role: bool,
    pub(super) tool: bool,
}

/// The audit log's list view.
///
/// This replaces a `"[{}] {:<8} {:<6} {}:{:<20} {}ms"` format string whose
/// padding was a literal rather than a function of the data, so a tool name
/// over 20 characters broke the column the operator was scanning, and no
/// header said what any column was (ADR-0306). Widths now come from the
/// result set and the sink; `DURATION (ms)` is a number column, so magnitudes
/// line up on their right edge and the unit is named once in the header
/// instead of being suffixed to every value.
pub(super) fn audit_event_table(events: &[AuditEvent], filters: AuditListFilters) -> Table {
    let mut table = Table::new(vec![
        Column::new("TIME").fixed(),
        Column::new("STATUS").fixed().filtered(filters.status),
        Column::new("ROLE").fixed().filtered(filters.role),
        Column::new("COMMAND").fixed(),
        Column::new("TOOL").filtered(filters.tool),
        Column::new("DURATION (ms)").number(),
    ])
    .empty_message("no audit events matching the given filters");

    for event in events {
        let status = event.status.to_string();
        table.add_row(vec![
            comfy_table::Cell::new(event.timestamp.format("%Y-%m-%dT%H:%M:%S").to_string()),
            cell(&status, Domain::AuditStatus),
            comfy_table::Cell::new(&event.role),
            comfy_table::Cell::new(&event.command),
            comfy_table::Cell::new(event.tool_name.as_deref().unwrap_or("-")),
            comfy_table::Cell::new(event.duration_ms.to_string()),
        ]);
    }

    table
}
