//! Store-level writers for the two rows worker supervision persists: a run's
//! terminal diagnostic step and a pipeline audit event.
//!
//! Both are plain store writes, so they take the backend they write through
//! rather than a runtime. [`OrbitRuntime`](crate::OrbitRuntime) and
//! [`PipelineWorkerSupervisor`](super::supervisor::PipelineWorkerSupervisor)
//! share these implementations instead of keeping two copies of the row shape.

use chrono::DateTime;
use orbit_store::contracts::{AuditEventStoreBackend, JobRunStoreBackend};

use super::*;

/// One pipeline audit row, as its callers describe it.
pub(crate) struct PipelineAuditRow<'a> {
    pub(crate) tool_name: &'a str,
    pub(crate) target_id: Option<&'a str>,
    pub(crate) actor: Option<&'a str>,
    pub(crate) status: AuditEventStatus,
    pub(crate) arguments: Value,
    pub(crate) error_message: Option<String>,
}

pub(crate) fn pipeline_audit(
    audit_events: &dyn AuditEventStoreBackend,
    working_directory: &Path,
    row: PipelineAuditRow<'_>,
) -> Result<(), OrbitError> {
    let arguments_json = serde_json::to_string(&row.arguments)
        .map_err(|error| OrbitError::Store(format!("serialize pipeline audit args: {error}")))?;
    let execution_id = audit_execution_id("exec");
    audit_events.insert_audit_event_record(&AuditEventInsertParams {
        execution_id,
        command: "tool".to_string(),
        subcommand: Some("run".to_string()),
        tool_name: Some(row.tool_name.to_string()),
        target_type: Some("job_run".to_string()),
        target_id: row.target_id.map(ToOwned::to_owned),
        role: "admin".to_string(),
        status: row.status,
        exit_code: if row.status == AuditEventStatus::Success {
            0
        } else {
            1
        },
        duration_ms: 0,
        working_directory: working_directory.display().to_string(),
        arguments_json: Some(arguments_json),
        stdout_truncated: None,
        stderr_truncated: None,
        error_message: row.error_message,
        host: row.actor.map(ToOwned::to_owned),
        pid: std::process::id(),
        session_id: None,
        workspace_id: None,
        caller_machine_id: None,
        caller_host_id: None,
        process_machine_id: None,
        process_host_id: None,
        transport: None,
        effective_capabilities: Default::default(),
        origin_session_id: None,
        mcp_call_id: None,
        lease_id: None,
        task_id: None,
        job_run_id: row.target_id.map(ToOwned::to_owned),
        activity_id: None,
        step_index: None,
    })
}

/// [ORB-10002] Record a terminal diagnostic step with an explicit state
/// (`failed` for job errors, `interrupted` for orphan reconciliation).
///
/// The first recorded error wins: a run that already carries one keeps it, so
/// a later supervisor observation cannot overwrite the cause the worker
/// itself reported.
pub(crate) fn diagnostic_step(
    runs: &dyn JobRunStoreBackend,
    run: &JobRun,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
    error_code: Option<&str>,
    message: &str,
    state: JobRunState,
) -> Result<(), OrbitError> {
    let current = runs
        .get_job_run(&run.run_id)?
        .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, run.run_id.clone()))?;
    let already_has_error = current
        .steps
        .iter()
        .any(|step| step.error_code.is_some() || step.error_message.is_some());
    if already_has_error {
        return Ok(());
    }

    let step_index = current
        .steps
        .iter()
        .map(|step| step.step_index)
        .max()
        .map(|index| index.saturating_add(1) as usize)
        .unwrap_or(0);
    let duration_ms = Some(
        finished_at
            .signed_duration_since(started_at)
            .num_milliseconds()
            .max(0) as u64,
    );
    let params = JobRunStepParams {
        step_index,
        target_type: JobTargetType::Job,
        target_id: run.job_id.clone(),
        started_at,
        finished_at,
        duration_ms,
        exit_code: None,
        agent_response_json: None,
        state,
        error_code: error_code.map(str::to_string),
        error_message: Some(message.to_string()),
    };
    let _ = runs.complete_job_run_step(&run.run_id, &params)?;
    Ok(())
}
