//! RAII SQLite CLI-invocation audit guard (`AuditGuard`); imports `command`, `orbit-common`, and `orbit-core`.

use std::ffi::OsStr;
use std::time::Instant;

use orbit_common::Env;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_core::adapter::command::take_tool_audit_recorded;
use orbit_core::app::InvocationAuditScope;
use orbit_core::{
    AuditEventInsertParams, AuditEventStatus, OrbitError, OrbitRuntime, redact_sensitive_env_text,
};

pub use crate::command::operation::CommandMeta;

/// Feeds the **persistent** SQLite audit event store after runtime bootstrap.
/// Bootstrap and preflight refusals do not instantiate this guard.
///
/// This is a separate mechanism from the in-process `EventLog` (`OrbitRuntime::event_log()`), which
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
    env: &'a Env,
    execution_id: String,
    meta: CommandMeta,
    start: Instant,
    status: AuditEventStatus,
    exit_code: i32,
    error_message: Option<String>,
    app_audit: Option<InvocationAuditScope>,
}

impl<'a> AuditGuard<'a> {
    pub fn new(runtime: &'a OrbitRuntime, meta: CommandMeta, env: &'a Env) -> Self {
        Self {
            runtime,
            env,
            execution_id: audit_execution_id("exec"),
            meta,
            start: Instant::now(),
            status: AuditEventStatus::Failure,
            exit_code: -1,
            error_message: None,
            app_audit: Some(InvocationAuditScope::enter()),
        }
    }

    pub fn mark_success(&mut self) {
        if let Some(scope) = self.app_audit.take() {
            scope.settle(Ok(()), 0, None);
        }
        self.status = AuditEventStatus::Success;
        self.exit_code = 0;
        self.error_message = None;
    }

    /// Classify the complete dispatch and rendering outcome. Rendering errors
    /// take precedence over a payload's declared exit code and audit message.
    pub fn mark_result(
        &mut self,
        result: &Result<(), OrbitError>,
        exit_code: i32,
        audit_message: Option<&str>,
    ) {
        if let Some(scope) = self.app_audit.take() {
            scope.settle(result.as_ref().copied(), exit_code, audit_message);
        }
        match result {
            Ok(()) if exit_code != 0 => {
                self.status = AuditEventStatus::Failure;
                self.exit_code = exit_code;
                self.error_message = audit_message.map(redact_sensitive_env_text);
            }
            Ok(_) => self.mark_success(),
            Err(OrbitError::PolicyDenied(msg) | OrbitError::CapabilityDenied(msg)) => {
                self.mark_denied(msg);
            }
            Err(error @ OrbitError::DecisionRefused { code, .. }) if code.is_policy_denial() => {
                self.mark_denied(&error.to_string());
            }
            Err(error @ OrbitError::NoticeRefused { code, .. }) if code.is_policy_denial() => {
                self.mark_denied(&error.to_string());
            }
            Err(err) => self.mark_failure(err),
        }
    }

    pub fn mark_failure(&mut self, error: &OrbitError) {
        if let Some(scope) = self.app_audit.take() {
            scope.settle(Err(error), 1, None);
        }
        self.status = AuditEventStatus::Failure;
        self.exit_code = 1;
        self.error_message = Some(redact_sensitive_env_text(&error.to_string()));
    }

    pub fn mark_denied(&mut self, msg: &str) {
        if let Some(scope) = self.app_audit.take() {
            let error = OrbitError::CapabilityDenied(msg.to_string());
            scope.settle(Err(&error), 1, None);
        }
        self.status = AuditEventStatus::Denied;
        self.exit_code = 1;
        self.error_message = Some(redact_sensitive_env_text(msg));
    }
}

impl Drop for AuditGuard<'_> {
    fn drop(&mut self) {
        // Flush any unsettled app invocation guard before checking
        // `take_tool_audit_recorded()`, so an early drop or panic unwind
        // during output rendering persists the app guard's `Failure / -1` row
        // and suppresses a duplicate emission here.
        drop(self.app_audit.take());

        // If `OrbitRuntime::execute_tool_command_dispatch` already persisted an
        // audit row for this thread (the runtime now owns tool-invocation audit
        // for both CLI and MCP entry points), suppress the guard's own emission
        // so we never double-audit a single `orbit tool run` invocation. Paths
        // that dispatch without executing a runtime tool call, such as
        // `--dry-run`, leave the flag clear and still get a guard-side row.
        // Invalid JSON is refused before bootstrap, before a guard exists.
        // An app's `exec` (`orbit_core::app::<subject>::exec`) sets the same
        // flag once it has written the invocation row of a command that ran
        // through it, so that command is audited once, by `exec`.
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
            host: self
                .env
                .hostname()
                .and_then(OsStr::to_str)
                .map(ToOwned::to_owned),
            pid: std::process::id(),
            session_id: None,
            workspace_id: self
                .runtime
                .workspace_runtime_binding()
                .map(|binding| binding.task_partition_id.clone()),
            caller_machine_id: None,
            caller_machine_name: None,
            process_machine_id: None,
            process_machine_name: None,
            transport: None,
            effective_capabilities: Default::default(),
            origin_session_id: None,
            mcp_call_id: None,
            lease_id: None,
            task_id: self
                .env
                .orbit_task_id()
                .and_then(OsStr::to_str)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned),
            job_run_id: self.meta.job_run_id.clone().or_else(|| {
                self.env
                    .orbit_run_id()
                    .and_then(OsStr::to_str)
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned)
            }),
            activity_id: self
                .env
                .orbit_activity_id()
                .and_then(OsStr::to_str)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned),
            step_index: self
                .env
                .orbit_step_index()
                .and_then(OsStr::to_str)
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
