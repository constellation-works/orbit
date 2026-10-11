//! Neutral coordination-hold audit seam.
//!
//! Task locks, workspace claims, and command execution all persist the same
//! audit shape. The types live here so that generic coordination infrastructure
//! is not owned by the task-lock module.

use orbit_common::OrbitError;
use orbit_types::telemetry::AuditEventStatus;
use serde_json::Value;

use super::system::{AuditRunAttribution, SystemAuditEvent, record_system_audit_event};
use crate::OrbitRuntime;

/// One coordination-hold audit event, before it is widened into a
/// [`SystemAuditEvent`].
///
/// [ORB-10709] `target_type` is carried here rather than fixed as a constant
/// because the workspace claim is a second coordination dimension over the same
/// table and must be distinguishable in the trail — a force-release has to be
/// legible as a claim event, not as a reservation event.
pub(crate) struct CoordinationAuditEvent<'a> {
    pub(crate) command: &'a str,
    pub(crate) tool_name: &'a str,
    pub(crate) target_type: &'a str,
    pub(crate) target_id: Option<&'a str>,
    pub(crate) task_id: Option<&'a str>,
    pub(crate) workspace_id: Option<&'a str>,
    pub(crate) status: AuditEventStatus,
    pub(crate) payload: Value,
}

/// Record one coordination-hold event (file reservation or workspace claim).
pub(crate) fn record_coordination_audit_event(
    runtime: &OrbitRuntime,
    event: CoordinationAuditEvent<'_>,
) -> Result<(), OrbitError> {
    let CoordinationAuditEvent {
        command,
        tool_name,
        target_type,
        target_id,
        task_id,
        workspace_id,
        status,
        payload,
    } = event;
    let execution_id_prefix = format!("audit-{}", command.replace('.', "-"));
    let run_id = owner_run_id_from_payload(&payload);
    let arguments_json = serde_json::to_string(&payload)
        .map_err(|error| OrbitError::Execution(format!("serialize audit payload: {error}")))?;
    record_system_audit_event(
        runtime,
        SystemAuditEvent {
            execution_id_prefix: &execution_id_prefix,
            command,
            subcommand: None,
            tool_name: Some(tool_name),
            target_type,
            target_id,
            role: "admin",
            status,
            arguments_json,
            workspace_id,
            task_id,
            attribution: AuditRunAttribution::Environment { run_id },
        },
    )
}

fn owner_run_id_from_payload(payload: &Value) -> Option<String> {
    payload
        .get("owner_run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}
