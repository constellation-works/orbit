//! The command-line invocation row and its drop guard.

use std::ffi::OsStr;
use std::time::Instant;

use orbit_common::OrbitError;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_common::security::redaction::redact_sensitive_env_text;
use orbit_store::AuditEventInsertParams;
use orbit_types::telemetry::AuditEventStatus;

use crate::app::exec::Operation;
use crate::{Core, Error};

/// An invocation audit guard held across a streaming command until the stream
/// terminates. Dropping it without calling [`Self::finish`] still writes a
/// failure row (`exit_code = -1`).
pub(in crate::app) struct StreamAudit(pub(in crate::app::exec) Option<AuditGuard>);

impl StreamAudit {
    /// Settle the stream's final outcome and write its invocation audit row.
    pub(in crate::app) fn finish(mut self, result: &Result<(), Error>) {
        if let Some(guard) = self.0.as_mut() {
            match result {
                Ok(()) => guard.finished(0),
                Err(error) => guard.failed(error),
            }
        }
    }
}

/// Writes one command's invocation audit row when dropped, so an early return
/// or a panic still leaves it. Until [`Self::finished`] is called the row says
/// the command failed with exit code -1.
pub(in crate::app) struct AuditGuard {
    runtime: crate::OrbitRuntime,
    operation: Operation,
    execution_id: String,
    start: Instant,
    status: AuditEventStatus,
    exit_code: i32,
    error_message: Option<String>,
}

impl AuditGuard {
    pub(in crate::app::exec) fn start(core: &Core, operation: Operation) -> Self {
        Self {
            runtime: core.runtime().clone(),
            operation,
            execution_id: audit_execution_id("exec"),
            start: Instant::now(),
            status: AuditEventStatus::Failure,
            exit_code: -1,
            error_message: None,
        }
    }

    fn row(&self) -> AuditEventInsertParams {
        let runtime = &self.runtime;
        let env = runtime.env();
        let env_text = |value: Option<&OsStr>| {
            value
                .and_then(OsStr::to_str)
                .filter(|text| !text.is_empty())
                .map(ToOwned::to_owned)
        };
        AuditEventInsertParams {
            subcommand: self
                .operation
                .recorded_subcommand
                .then(|| self.operation.subcommand.clone()),
            tool_name: self.operation.recorded_tool_name.clone(),
            target_type: self.operation.target_type.map(ToOwned::to_owned),
            target_id: self.operation.target_id.clone(),
            role: self
                .operation
                .audit_role
                .map_or_else(|| runtime.actor().audit_role().to_string(), str::to_owned),
            status: self.status,
            exit_code: self.exit_code,
            duration_ms: self.start.elapsed().as_millis() as i64,
            working_directory: std::env::current_dir()
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string()),
            arguments_json: self.operation.arguments_json.clone(),
            error_message: self.error_message.clone(),
            host: env
                .hostname()
                .and_then(OsStr::to_str)
                .map(ToOwned::to_owned),
            workspace_id: runtime
                .workspace_runtime_binding()
                .map(|binding| binding.task_partition_id.clone()),
            task_id: env_text(env.orbit_task_id()),
            job_run_id: self
                .operation
                .job_run_id
                .clone()
                .or_else(|| env_text(env.orbit_run_id())),
            activity_id: env_text(env.orbit_activity_id()),
            step_index: env
                .orbit_step_index()
                .and_then(OsStr::to_str)
                .and_then(|text| text.parse().ok()),
            ..super::unattributed_row(self.execution_id.clone(), self.operation.command)
        }
    }

    /// Record that the command ran and answers `exit_code`.
    pub(in crate::app) fn finished(&mut self, exit_code: i32) {
        self.finished_with_message(exit_code, None);
    }

    /// Record that the command ran and answers `exit_code`, optionally carrying
    /// a redacted audit message when `exit_code != 0`.
    pub(in crate::app) fn finished_with_message(
        &mut self,
        exit_code: i32,
        audit_message: Option<&str>,
    ) {
        self.status = match exit_code {
            0 => AuditEventStatus::Success,
            _ => AuditEventStatus::Failure,
        };
        self.exit_code = exit_code;
        self.error_message = (exit_code != 0)
            .then_some(audit_message)
            .flatten()
            .map(redact_sensitive_env_text);
    }

    /// Record the error the command ended with.
    pub(in crate::app) fn failed(&mut self, error: &Error) {
        let denial = match error {
            OrbitError::PolicyDenied(message) | OrbitError::CapabilityDenied(message) => {
                Some(message.clone())
            }
            OrbitError::AuthorizationDenied(_) => Some(error.to_string()),
            OrbitError::DecisionRefused { code, .. } if code.is_policy_denial() => {
                Some(error.to_string())
            }
            OrbitError::NoticeRefused { code, .. } if code.is_policy_denial() => {
                Some(error.to_string())
            }
            _ => None,
        };
        self.status = match denial {
            Some(_) => AuditEventStatus::Denied,
            None => AuditEventStatus::Failure,
        };
        self.exit_code = 1;
        self.error_message = Some(redact_sensitive_env_text(
            &denial.unwrap_or_else(|| error.to_string()),
        ));
    }
}

impl Drop for AuditGuard {
    fn drop(&mut self) {
        let row = self.row();
        let written = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::app::audit::record_audit_event(&self.runtime, &row)
        }));
        // A row that was not written stays owed: the command line's
        // invocation scope then writes it, and reports the failure if it
        // fails again.
        if matches!(written, Ok(Ok(()))) {
            crate::adapter::command::mark_tool_audit_recorded();
        }
    }
}
