use orbit_core::AuditEvent;
use orbit_types::telemetry::audit_event_base_json;
use serde_json::{Value, json};

use crate::output::color::{Domain, cell};
use crate::output::table::{Column, Table};

pub(super) fn audit_event_to_json(event: &AuditEvent) -> Value {
    let mut value = audit_event_base_json(event);
    // ORB-10890: named `self_reported_*` rather than `actor` precisely so
    // a consumer cannot mistake it for the authenticated `role` above.
    value["self_reported_actor"] = json!(event.self_reported_actor);
    value["plugin"] = json!(event.plugin);
    value["plugin_secrets"] = json!(event.plugin_secrets);
    value["plugin_secret_updates"] = json!(event.plugin_secret_updates);
    value["brokered"] = json!(event.brokered);
    value["peer_pid"] = json!(event.peer_pid);
    value
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
