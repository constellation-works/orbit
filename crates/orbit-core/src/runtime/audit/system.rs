//! System-authored audit rows.
//!
//! Orbit records some audit events on its own behalf rather than for a caller
//! request: coordination holds, artifact redactions, auto-task lifecycle
//! changes and pipeline gate outcomes. They carry no caller, session or
//! transport context, so the columns those would fill are constant here and
//! callers state only what varies.

use orbit_common::OrbitError;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_types::telemetry::AuditEventStatus;

use crate::{AuditEventInsertParams, OrbitRuntime};

/// Where a system audit row takes its run attribution from.
pub(crate) enum AuditRunAttribution {
    /// The given run, with no activity or step.
    Run(Option<String>),
    /// The executing activity, read from `ORBIT_RUN_ID`, `ORBIT_ACTIVITY_ID`
    /// and `ORBIT_STEP_INDEX`. A `run_id` given here takes precedence over
    /// `ORBIT_RUN_ID`.
    Environment { run_id: Option<String> },
}

/// One system-authored audit event, before it is widened into the full
/// `AuditEventInsertParams`.
pub(crate) struct SystemAuditEvent<'a> {
    pub(crate) execution_id_prefix: &'a str,
    pub(crate) command: &'a str,
    pub(crate) subcommand: Option<&'a str>,
    pub(crate) tool_name: Option<&'a str>,
    pub(crate) target_type: &'a str,
    pub(crate) target_id: Option<&'a str>,
    pub(crate) role: &'a str,
    /// `Denied` records exit code 1; every other status records 0.
    pub(crate) status: AuditEventStatus,
    pub(crate) arguments_json: String,
    pub(crate) workspace_id: Option<&'a str>,
    pub(crate) task_id: Option<&'a str>,
    pub(crate) attribution: AuditRunAttribution,
}

/// Record one system-authored audit event.
pub(crate) fn record_system_audit_event(
    runtime: &OrbitRuntime,
    event: SystemAuditEvent<'_>,
) -> Result<(), OrbitError> {
    let SystemAuditEvent {
        execution_id_prefix,
        command,
        subcommand,
        tool_name,
        target_type,
        target_id,
        role,
        status,
        arguments_json,
        workspace_id,
        task_id,
        attribution,
    } = event;
    let (job_run_id, activity_id, step_index) = match attribution {
        AuditRunAttribution::Run(run_id) => (run_id, None, None),
        AuditRunAttribution::Environment { run_id } => (
            run_id.or_else(|| non_empty_env("ORBIT_RUN_ID")),
            non_empty_env("ORBIT_ACTIVITY_ID"),
            std::env::var("ORBIT_STEP_INDEX")
                .ok()
                .and_then(|s| s.parse().ok()),
        ),
    };
    runtime.record_audit_event(&AuditEventInsertParams {
        execution_id: audit_execution_id(execution_id_prefix),
        command: command.to_string(),
        subcommand: subcommand.map(ToOwned::to_owned),
        tool_name: tool_name.map(ToOwned::to_owned),
        target_type: Some(target_type.to_string()),
        target_id: target_id.map(ToOwned::to_owned),
        role: role.to_string(),
        status,
        exit_code: if status == AuditEventStatus::Denied {
            1
        } else {
            0
        },
        duration_ms: 0,
        working_directory: runtime.paths().repo_root.to_string_lossy().into_owned(),
        arguments_json: Some(arguments_json),
        stdout_truncated: None,
        stderr_truncated: None,
        error_message: None,
        host: std::env::var("HOSTNAME").ok(),
        pid: std::process::id(),
        session_id: None,
        workspace_id: workspace_id.map(ToOwned::to_owned),
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
        activity_id,
        step_index,
    })
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
}
