use std::time::Instant;

use orbit_common::observability::audit_id::audit_execution_id;
use orbit_core::adapter::command::take_tool_audit_recorded;
use orbit_core::{
    AuditEventInsertParams, AuditEventStatus, OrbitError, OrbitRuntime, redact_sensitive_env_text,
};

use crate::command::CommandOut;
#[cfg(test)]
use crate::command::Commands;
pub use crate::command::operation::CommandMeta;

/// Feeds the **persistent** SQLite audit event store on every CLI invocation.
///
/// This is a separate mechanism from the in-process `EventLog` (`OrbitRuntime.event_log`), which
/// is session-scoped and not persisted. The two channels serve different purposes:
/// - `AuditGuard` (this file): records structured CLI invocation metadata to SQLite; survives
///   process restarts; queryable via `orbit audit list`.
/// - `EventLog`: records in-memory `OrbitEvent` mutations for the current session only; used for
///   internal runtime tracking, not for persistent audit history.
///
/// RAII audit guard that writes an audit record on scope exit via `Drop`.
///
/// Guarantees exactly one audit record per command execution — even on
/// early returns or panics (with `panic = "unwind"`).
///
/// Status defaults to `Failure` with exit code -1 if never explicitly marked.
pub struct AuditGuard<'a> {
    runtime: &'a OrbitRuntime,
    execution_id: String,
    meta: CommandMeta,
    start: Instant,
    status: AuditEventStatus,
    exit_code: i32,
    error_message: Option<String>,
}

impl<'a> AuditGuard<'a> {
    pub fn new(runtime: &'a OrbitRuntime, meta: CommandMeta) -> Self {
        Self {
            runtime,
            execution_id: audit_execution_id("exec"),
            meta,
            start: Instant::now(),
            status: AuditEventStatus::Failure,
            exit_code: -1,
            error_message: None,
        }
    }

    pub fn mark_success(&mut self) {
        self.status = AuditEventStatus::Success;
        self.exit_code = 0;
        self.error_message = None;
    }

    /// Classify the command's complete outcome, including rendered payloads
    /// that deliberately request a nonzero process exit.
    pub fn mark_result(&mut self, result: &CommandOut) {
        match result {
            Ok(output) if output.exit_code() != 0 => {
                self.status = AuditEventStatus::Failure;
                self.exit_code = output.exit_code();
                self.error_message = None;
            }
            Ok(_) => self.mark_success(),
            Err(OrbitError::PolicyDenied(msg) | OrbitError::CapabilityDenied(msg)) => {
                self.mark_denied(msg);
            }
            Err(err) => self.mark_failure(err),
        }
    }

    pub fn mark_failure(&mut self, error: &OrbitError) {
        self.status = AuditEventStatus::Failure;
        self.exit_code = 1;
        self.error_message = Some(redact_sensitive_env_text(&error.to_string()));
    }

    pub fn mark_denied(&mut self, msg: &str) {
        self.status = AuditEventStatus::Denied;
        self.exit_code = 1;
        self.error_message = Some(redact_sensitive_env_text(msg));
    }
}

impl Drop for AuditGuard<'_> {
    fn drop(&mut self) {
        // If `OrbitRuntime::execute_tool_command_dispatch` already persisted an
        // audit row for this thread (the runtime now owns tool-invocation audit
        // for both CLI and MCP entry points), suppress the guard's own emission
        // so we never double-audit a single `orbit tool run` invocation. Paths
        // that bail before the runtime is reached — invalid JSON, missing
        // input, `--dry-run` — leave the flag clear and still get a guard-side
        // row.
        if take_tool_audit_recorded() {
            return;
        }

        let duration_ms = self.start.elapsed().as_millis() as i64;

        let working_directory = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string());

        let params = AuditEventInsertParams {
            execution_id: self.execution_id.clone(),
            command: self.meta.command.clone(),
            subcommand: self.meta.subcommand.clone(),
            tool_name: self.meta.tool_name.clone(),
            target_type: self.meta.target_type.clone(),
            target_id: self.meta.target_id.clone(),
            role: self.meta.role.clone(),
            status: self.status,
            exit_code: self.exit_code,
            duration_ms,
            working_directory,
            arguments_json: self.meta.arguments_json.clone(),
            stdout_truncated: None,
            stderr_truncated: None,
            error_message: self.error_message.clone(),
            host: std::env::var("HOSTNAME").ok(),
            pid: std::process::id(),
            session_id: None,
            workspace_id: self
                .runtime
                .workspace_runtime_binding()
                .map(|binding| binding.task_partition_id.clone()),
            caller_machine_id: None,
            caller_host_id: None,
            process_machine_id: None,
            process_host_id: None,
            transport: None,
            effective_capabilities: Default::default(),
            origin_session_id: None,
            mcp_call_id: None,
            lease_id: None,
            task_id: std::env::var("ORBIT_TASK_ID")
                .ok()
                .filter(|s| !s.is_empty()),
            job_run_id: self
                .meta
                .job_run_id
                .clone()
                .or_else(|| std::env::var("ORBIT_RUN_ID").ok().filter(|s| !s.is_empty())),
            activity_id: std::env::var("ORBIT_ACTIVITY_ID")
                .ok()
                .filter(|s| !s.is_empty()),
            step_index: std::env::var("ORBIT_STEP_INDEX")
                .ok()
                .and_then(|s| s.parse().ok()),
        };

        let write_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.runtime.record_audit_event(&params)
        }));

        match write_result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                eprintln!("warning: failed to write audit event: {e}");
            }
            Err(_) => {
                eprintln!("critical: audit panic during drop");
            }
        }
    }
}

/// Compatibility adapter for focused audit tests and callers. Metadata now
/// comes from the command-operation registry instead of a second command tree.
#[cfg(test)]
pub fn extract_command_meta(command: &Commands) -> CommandMeta {
    command
        .operation()
        .audit_meta
        .unwrap_or_else(|| unreachable!("audit commands do not emit audit metadata"))
}
