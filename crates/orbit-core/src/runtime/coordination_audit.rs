//! Neutral coordination-hold audit seam.
//!
//! Task locks, workspace claims, and command execution all persist the same
//! audit shape. The types live here so that generic coordination infrastructure
//! is not owned by the task-lock module.

use orbit_common::OrbitError;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_types::telemetry::AuditEventStatus;
use serde_json::Value;

use crate::OrbitRuntime;

/// One coordination-hold audit event, before it is widened into the full
/// `AuditEventInsertParams`.
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
        status,
        payload,
    } = event;
    let execution_id_prefix = format!("audit-{}", command.replace('.', "-"));
    let job_run_id = owner_run_id_from_payload(&payload)
        .or_else(|| std::env::var("ORBIT_RUN_ID").ok().filter(|s| !s.is_empty()));
    runtime.record_audit_event(&crate::AuditEventInsertParams {
        execution_id: audit_execution_id(&execution_id_prefix),
        command: command.to_string(),
        subcommand: None,
        tool_name: Some(tool_name.to_string()),
        target_type: Some(target_type.to_string()),
        target_id: target_id.map(ToOwned::to_owned),
        role: "admin".to_string(),
        status,
        exit_code: if status == AuditEventStatus::Denied {
            1
        } else {
            0
        },
        duration_ms: 0,
        working_directory: runtime.paths().repo_root.to_string_lossy().into_owned(),
        arguments_json: Some(
            serde_json::to_string(&payload).map_err(|error| {
                OrbitError::Execution(format!("serialize audit payload: {error}"))
            })?,
        ),
        stdout_truncated: None,
        stderr_truncated: None,
        error_message: None,
        host: std::env::var("HOSTNAME").ok(),
        pid: std::process::id(),
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
        task_id: task_id.map(ToOwned::to_owned),
        job_run_id,
        activity_id: std::env::var("ORBIT_ACTIVITY_ID")
            .ok()
            .filter(|s| !s.is_empty()),
        step_index: std::env::var("ORBIT_STEP_INDEX")
            .ok()
            .and_then(|s| s.parse().ok()),
    })
}

fn owner_run_id_from_payload(payload: &Value) -> Option<String> {
    payload
        .get("owner_run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}
