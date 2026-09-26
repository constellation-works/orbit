//! Row hydration shared by every audit-event query.

use std::collections::BTreeSet;

use orbit_types::plugin::PluginProvenance;
use orbit_types::telemetry::{AuditEvent, AuditEventStatus};
use orbit_types::tool::{McpCapability, McpTransport};

use crate::parse_timestamp;

/// Canonical `SELECT` column list for a full [`AuditEvent`] row, in the order
/// [`audit_event_from_row`] indexes. Shared by every query that hydrates whole
/// events so a schema column can only be added in one place.
pub(super) const AUDIT_EVENT_COLUMNS: &str = "id, execution_id, timestamp, command, subcommand, \
     tool_name, target_type, target_id, role, status, exit_code, duration_ms, \
     working_directory, arguments_json, stdout_truncated, stderr_truncated, \
     error_message, host, pid, session_id, workspace_id, caller_machine_id, \
     caller_machine_name, process_machine_id, process_machine_name, transport, \
     capabilities_json, origin_session_id, mcp_call_id, lease_id, task_id, \
     job_run_id, activity_id, step_index, trace_id, caller_ip, \
     self_reported_actor, plugin_name, plugin_version, plugin_manifest_digest, \
     plugin_grants, plugin_secrets, plugin_secret_updates";

pub(super) fn audit_event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuditEvent> {
    let ts_raw: String = row.get(2)?;
    let status_raw: String = row.get(9)?;
    let timestamp = parse_timestamp(&ts_raw)?;
    let status = status_raw
        .parse::<AuditEventStatus>()
        .map_err(|error| invalid_text(9, &status_raw, error))?;
    let transport_raw: Option<String> = row.get(25)?;
    let transport = transport_raw
        .as_deref()
        .map(str::parse::<McpTransport>)
        .transpose()
        .map_err(|error| invalid_text(25, transport_raw.as_deref().unwrap_or_default(), error))?;
    let capabilities_raw: Option<String> = row.get(26)?;
    let effective_capabilities = capabilities_raw
        .as_deref()
        .map(serde_json::from_str::<BTreeSet<McpCapability>>)
        .transpose()
        .map_err(|error| {
            invalid_text(
                26,
                capabilities_raw.as_deref().unwrap_or_default(),
                error.to_string(),
            )
        })?
        .unwrap_or_default();

    Ok(AuditEvent {
        id: row.get(0)?,
        execution_id: row.get(1)?,
        timestamp,
        command: row.get(3)?,
        subcommand: row.get(4)?,
        tool_name: row.get(5)?,
        target_type: row.get(6)?,
        target_id: row.get(7)?,
        role: row.get(8)?,
        status,
        exit_code: row.get(10)?,
        duration_ms: row.get(11)?,
        working_directory: row.get(12)?,
        arguments_json: row.get(13)?,
        stdout_truncated: row.get(14)?,
        stderr_truncated: row.get(15)?,
        error_message: row.get(16)?,
        host: row.get(17)?,
        pid: row.get(18)?,
        session_id: row.get(19)?,
        workspace_id: row.get(20)?,
        caller_machine_id: row.get(21)?,
        caller_machine_name: row.get(22)?,
        process_machine_id: row.get(23)?,
        process_machine_name: row.get(24)?,
        transport,
        effective_capabilities,
        origin_session_id: row.get(27)?,
        mcp_call_id: row.get(28)?,
        trace_id: row.get(34)?,
        caller_ip: row.get(35)?,
        lease_id: row.get(29)?,
        task_id: row.get(30)?,
        job_run_id: row.get(31)?,
        activity_id: row.get(32)?,
        step_index: row.get(33)?,
        self_reported_actor: row.get(36)?,
        plugin: plugin_provenance_from_row(row)?,
        plugin_secrets: row
            .get::<_, Option<String>>(41)?
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default(),
        plugin_secret_updates: row
            .get::<_, Option<String>>(42)?
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default(),
    })
}

/// The three plugin columns are written together, so a row either names a
/// plugin completely or names none.
fn plugin_provenance_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Option<PluginProvenance>> {
    let name: Option<String> = row.get(37)?;
    let version: Option<String> = row.get(38)?;
    let manifest_digest: Option<String> = row.get(39)?;
    let grants: Option<String> = row.get(40)?;
    Ok(match (name, version, manifest_digest) {
        (Some(name), Some(version), Some(manifest_digest)) => Some(PluginProvenance {
            name,
            version,
            manifest_digest,
            grants: grants
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_default(),
        }),
        _ => None,
    })
}

pub(super) fn invalid_text(index: usize, raw: &str, error: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} ({raw})", error.into()),
        )),
    )
}
