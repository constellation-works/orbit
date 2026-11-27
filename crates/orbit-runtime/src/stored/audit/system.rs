//! System-authored audit rows.
//!
//! Orbit records some audit events on its own behalf rather than for a caller
//! request: coordination holds, artifact redactions, auto-task lifecycle
//! changes and pipeline gate outcomes. They carry no caller, session or
//! transport context, so the columns those would fill are constant here and
//! callers state only what varies.

use orbit_common::OrbitError;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_common::security::redaction::{redact_all, redact_all_json_with_keys};
use orbit_types::telemetry::AuditEventStatus;

use crate::RuntimeKernel;
use orbit_store::AuditEventInsertParams;
use orbit_store::contract::AuditInvocationFields;

/// Where a system audit row takes its run attribution from.
pub enum AuditRunAttribution {
    /// The given run, with no activity or step.
    Run(Option<String>),
    /// The executing activity, read from `ORBIT_RUN_ID`, `ORBIT_ACTIVITY_ID`
    /// and `ORBIT_STEP_INDEX`. A `run_id` given here takes precedence over
    /// `ORBIT_RUN_ID`.
    Environment { run_id: Option<String> },
}

/// One system-authored audit event, before it is widened into the full
/// `AuditEventInsertParams`.
pub struct SystemAuditEvent<'a> {
    pub execution_id_prefix: &'a str,
    pub command: &'a str,
    pub subcommand: Option<&'a str>,
    pub tool_name: Option<&'a str>,
    pub target_type: &'a str,
    pub target_id: Option<&'a str>,
    pub role: &'a str,
    /// `Denied` records exit code 1; every other status records 0.
    pub status: AuditEventStatus,
    /// Serialized JSON arguments. Redacted before they are persisted.
    pub arguments_json: String,
    /// Optional diagnostic stored separately from the event's arguments.
    pub error_message: Option<String>,
    pub workspace_id: Option<&'a str>,
    pub task_id: Option<&'a str>,
    pub attribution: AuditRunAttribution,
}

/// Record one system-authored audit event.
pub fn record_system_audit_event(
    runtime: &RuntimeKernel,
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
        error_message,
        workspace_id,
        task_id,
        attribution,
    } = event;
    let (job_run_id, activity_id, step_index) = match attribution {
        AuditRunAttribution::Run(run_id) => (run_id, None, None),
        AuditRunAttribution::Environment { run_id } => (
            run_id.or_else(|| non_empty_env(runtime.env().orbit_run_id())),
            non_empty_env(runtime.env().orbit_activity_id()),
            runtime
                .env()
                .orbit_step_index()
                .and_then(std::ffi::OsStr::to_str)
                .and_then(|s| s.parse().ok()),
        ),
    };
    runtime.record_audit_event_with_invocation(
        &AuditEventInsertParams {
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
            arguments_json: Some(durable_arguments_json(&arguments_json)),
            stdout_truncated: None,
            stderr_truncated: None,
            error_message: error_message.map(|message| redact_all(&message)),
            host: runtime
                .env()
                .hostname()
                .and_then(std::ffi::OsStr::to_str)
                .map(ToOwned::to_owned),
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
        },
        AuditInvocationFields::default(),
    )
}

/// Redact arguments at the durability boundary. Callers pass serialized JSON,
/// so it is redacted as a document: every string value, and every property
/// name, gets the same redaction as a refused tool call's arguments. Text that
/// is not JSON falls back to the text redactor, so it is never stored raw.
fn durable_arguments_json(arguments_json: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(arguments_json) {
        Ok(arguments) => redact_all_json_with_keys(arguments).to_string(),
        Err(_) => redact_all(arguments_json),
    }
}

fn non_empty_env(value: Option<&std::ffi::OsStr>) -> Option<String> {
    value
        .and_then(std::ffi::OsStr::to_str)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}
