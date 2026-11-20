//! The job-run audit trail's writers: a run's terminal diagnostic step and a
//! pipeline audit event.
//!
//! Both are plain store writes, so the free functions take the backend they
//! write through rather than a runtime. [`OrbitRuntime`] and the pipeline's
//! worker supervisor share these implementations instead of keeping two
//! copies of the row shape. The run owns the audit trail, so `job/run` sits
//! below `job/pipeline`: the pipeline imports these writers, never the
//! reverse. The two [`OrbitRuntime`] methods forward to them for the callers
//! that already hold a runtime (`gc`, `review`, `job/exec`, orphan adoption,
//! upgrade resume, composition and the run controls).

use std::path::Path;

use chrono::{DateTime, Utc};
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::contracts::{
    AuditEventInsertParams, AuditEventStoreBackend, JobRunStepParams, JobRunStoreBackend,
};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{JobRun, JobRunState, JobTargetType};
use serde_json::Value;

use crate::OrbitRuntime;

/// One pipeline audit row, as its callers describe it.
pub(crate) struct PipelineAuditRow<'a> {
    pub(crate) tool_name: &'a str,
    pub(crate) target_id: Option<&'a str>,
    pub(crate) actor: Option<&'a str>,
    pub(crate) status: AuditEventStatus,
    pub(crate) arguments: Value,
    pub(crate) error_message: Option<String>,
    /// The logical workspace the row belongs to, for a decision about the
    /// workspace rather than one run; `None` for a run's own rows.
    pub(crate) workspace_id: Option<String>,
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
        workspace_id: row.workspace_id,
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

    write_diagnostic_step(
        runs,
        &current,
        started_at,
        finished_at,
        error_code,
        message,
        state,
    )
}

/// A watchdog cause must remain visible even when an earlier activity failed.
/// Linux scope diagnostics retain their existing first-error policy. Only the
/// macOS watchdog calls it; it compiles on Linux with the watchdog module.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "only the macOS watchdog calls this; Linux compiles it for CodeQL extraction and never starts a watchdog"
    )
)]
pub(crate) fn watchdog_diagnostic_step(
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
    write_diagnostic_step(
        runs,
        &current,
        started_at,
        finished_at,
        error_code,
        message,
        state,
    )
}

fn write_diagnostic_step(
    runs: &dyn JobRunStoreBackend,
    current: &JobRun,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
    error_code: Option<&str>,
    message: &str,
    state: JobRunState,
) -> Result<(), OrbitError> {
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
        lock_wait_ms: None,
        step_index,
        target_type: JobTargetType::Job,
        target_id: current.job_id.clone(),
        started_at,
        finished_at,
        duration_ms,
        exit_code: None,
        agent_response_json: None,
        state,
        error_code: error_code.map(str::to_string),
        error_message: Some(message.to_string()),
    };
    let _ = runs.complete_job_run_step(&current.run_id, &params)?;
    Ok(())
}

impl OrbitRuntime {
    /// [ORB-10002] Record a terminal diagnostic step with an explicit state
    /// (`failed` for job errors, `interrupted` for orphan reconciliation).
    pub(crate) fn record_pipeline_diagnostic_step(
        &self,
        run: &JobRun,
        started_at: DateTime<Utc>,
        finished_at: DateTime<Utc>,
        error_code: Option<&str>,
        message: &str,
        state: JobRunState,
    ) -> Result<(), OrbitError> {
        diagnostic_step(
            self.stores().jobs(),
            run,
            started_at,
            finished_at,
            error_code,
            message,
            state,
        )
    }

    /// Record one pipeline audit event for a job run.
    pub(crate) fn record_pipeline_audit(
        &self,
        tool_name: &str,
        target_id: Option<&str>,
        actor: Option<&str>,
        status: AuditEventStatus,
        arguments: Value,
        error_message: Option<String>,
    ) -> Result<(), OrbitError> {
        pipeline_audit(
            self.stores().audit_events(),
            &self.paths().repo_root,
            PipelineAuditRow {
                tool_name,
                target_id,
                actor,
                status,
                arguments,
                error_message,
                workspace_id: None,
            },
        )
    }

    /// Record one pipeline audit event about this workspace as a whole,
    /// stamped with its logical workspace id so a read can scope to it.
    pub(crate) fn record_workspace_pipeline_audit(
        &self,
        row: PipelineAuditRow<'_>,
    ) -> Result<(), OrbitError> {
        let row = PipelineAuditRow {
            workspace_id: Some(self.workspace_id()?),
            ..row
        };
        pipeline_audit(self.stores().audit_events(), &self.paths().repo_root, row)
    }
}
