//! One command-line invocation's audit row, across dispatch and rendering.
//!
//! A command line enters an [`InvocationAuditScope`] once its workspace is
//! open and settles it after rendering. An `exec` that succeeds inside the
//! scope defers its invocation row here, so the row records the rendered
//! outcome. An invocation that never reaches an `exec` (a command line
//! refused its input first, or a tool call the runtime ran without writing a
//! row) is written by the scope itself, from the [`CommandMeta`] the command
//! line declared, so every invocation leaves exactly one row.

use std::cell::RefCell;
use std::ffi::OsStr;
use std::fmt;
use std::time::Instant;

use orbit_common::OrbitError;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_common::security::redaction::redact_sensitive_env_text;
use orbit_store::AuditEventInsertParams;
use orbit_types::telemetry::AuditEventStatus;

use super::exec::AuditGuard;
use crate::adapter::command::take_tool_audit_recorded;
use crate::{Core, Error, OrbitRuntime};

thread_local! {
    /// Active CLI invocation audit settlement scope. `Some(None)` means the
    /// CLI entry is ready to settle one deferred `AuditGuard` after output
    /// rendering completes; `Some(Some(_))` holds the unsettled guard.
    static DEFERRED_INVOCATION_AUDIT: RefCell<Option<Option<Box<DeferredAudit>>>> =
        const { RefCell::new(None) };
}

/// What a command line declares about one invocation: the fields of the
/// invocation row the scope writes when no `exec` writes one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandMeta {
    /// The top-level command.
    pub command: String,
    /// The subcommand, when the command has one.
    pub subcommand: Option<String>,
    /// The tool the invocation calls, when it calls one.
    pub tool_name: Option<String>,
    /// The kind of record the invocation targets.
    pub target_type: Option<String>,
    /// The record the invocation targets.
    pub target_id: Option<String>,
    /// The audit role the invocation acts under.
    pub role: String,
    /// The invocation's arguments, as JSON text.
    pub arguments_json: Option<String>,
    /// The run the invocation belongs to, where the command names one.
    pub job_run_id: Option<String>,
}

/// Why a scope's own invocation row was not written. Its `Display` is the
/// line a command line reports on stderr.
#[derive(Debug)]
pub enum AuditWriteFailure {
    /// The store refused the row.
    Failed(Error),
    /// Writing the row panicked.
    Panicked,
}

impl fmt::Display for AuditWriteFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Failed(error) => {
                write!(formatter, "warning: failed to write audit event: {error}")
            }
            Self::Panicked => formatter.write_str("critical: audit panic during drop"),
        }
    }
}

/// Scopes command-line invocation audit settlement across command dispatch and
/// CLI output rendering.
///
/// While active on the current thread, a command-line app `exec` that succeeds
/// defers its invocation audit guard here instead of writing `success` before
/// stdout rendering completes. [`Self::settle`] records the final render
/// result, exit code and optional audit message and flushes the audit row
/// before process exit; dropping the scope without settling (for example, on
/// panic unwind when stdout fails) drops the guard unsettled so it persists
/// `failure` with `exit_code = -1`.
///
/// When no `exec` (and no tool call) recorded a row for the invocation, the
/// scope writes one from its [`CommandMeta`], with the same outcome.
pub struct InvocationAuditScope {
    active: bool,
    own_row: Option<OwnRow>,
}

impl InvocationAuditScope {
    /// Begin deferring command-line invocation audit settlement on this thread
    /// until [`Self::settle`] or drop, for the invocation `meta` declares on
    /// the workspace `core` opened.
    pub fn enter(core: &Core, meta: CommandMeta) -> Self {
        let active = DEFERRED_INVOCATION_AUDIT.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_some() {
                return false;
            }
            *slot = Some(None);
            true
        });
        Self {
            active,
            own_row: Some(OwnRow::start(core.runtime().clone(), meta)),
        }
    }

    fn take_deferred(&mut self) -> Option<Box<DeferredAudit>> {
        if !self.active {
            return None;
        }
        self.active = false;
        DEFERRED_INVOCATION_AUDIT.with(|slot| slot.borrow_mut().take().flatten())
    }

    /// Settle the invocation with the complete dispatch and rendering
    /// outcome, flushing its row before returning.
    ///
    /// Rendering errors take precedence over the command's or payload's
    /// declared exit code and audit message. An error is returned only when
    /// the scope's own row could not be written.
    pub fn settle(
        mut self,
        result: Result<(), &Error>,
        exit_code: i32,
        audit_message: Option<&str>,
    ) -> Result<(), AuditWriteFailure> {
        if let Some(mut deferred) = self.take_deferred() {
            match result {
                Ok(()) => {
                    let effective_exit_code = if exit_code != 0 {
                        exit_code
                    } else {
                        deferred.exit_code
                    };
                    deferred
                        .guard
                        .finished_with_message(effective_exit_code, audit_message);
                }
                Err(error) => deferred.guard.failed(error),
            }
        }
        let Some(mut row) = self.own_row.take() else {
            return Ok(());
        };
        row.settle(result, exit_code, audit_message);
        row.write()
    }
}

impl Drop for InvocationAuditScope {
    fn drop(&mut self) {
        // Flush any unsettled `exec` guard before the scope's own row checks
        // whether one was recorded, so an early drop or a panic unwind during
        // rendering persists that guard's `failure / -1` row and no second one.
        drop(self.take_deferred());
        if let Some(row) = self.own_row.take()
            && let Err(failure) = row.write()
        {
            tracing::error!(%failure, "invocation audit row not written");
        }
    }
}

struct DeferredAudit {
    guard: AuditGuard,
    exit_code: i32,
}

/// The row an invocation is owed when no `exec` writes one. Until it is
/// settled it says the command failed with exit code -1.
struct OwnRow {
    runtime: OrbitRuntime,
    meta: CommandMeta,
    execution_id: String,
    start: Instant,
    status: AuditEventStatus,
    exit_code: i32,
    error_message: Option<String>,
}

impl OwnRow {
    fn start(runtime: OrbitRuntime, meta: CommandMeta) -> Self {
        Self {
            runtime,
            meta,
            execution_id: audit_execution_id("exec"),
            start: Instant::now(),
            status: AuditEventStatus::Failure,
            exit_code: -1,
            error_message: None,
        }
    }

    fn row(&self) -> AuditEventInsertParams {
        let env = self.runtime.env();
        let env_text = |value: Option<&OsStr>| {
            value
                .and_then(OsStr::to_str)
                .filter(|text| !text.is_empty())
                .map(ToOwned::to_owned)
        };
        AuditEventInsertParams {
            execution_id: self.execution_id.clone(),
            command: self.meta.command.clone(),
            subcommand: self.meta.subcommand.clone(),
            tool_name: self.meta.tool_name.clone(),
            target_type: self.meta.target_type.clone(),
            target_id: self.meta.target_id.clone(),
            role: self.meta.role.clone(),
            status: self.status,
            exit_code: self.exit_code,
            duration_ms: self.start.elapsed().as_millis() as i64,
            working_directory: std::env::current_dir()
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string()),
            arguments_json: self.meta.arguments_json.clone(),
            stdout_truncated: None,
            stderr_truncated: None,
            error_message: self.error_message.clone(),
            host: env
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
            task_id: env_text(env.orbit_task_id()),
            job_run_id: self
                .meta
                .job_run_id
                .clone()
                .or_else(|| env_text(env.orbit_run_id())),
            activity_id: env_text(env.orbit_activity_id()),
            step_index: env
                .orbit_step_index()
                .and_then(OsStr::to_str)
                .and_then(|text| text.parse().ok()),
        }
    }

    /// Classify the complete dispatch and rendering outcome.
    fn settle(&mut self, result: Result<(), &Error>, exit_code: i32, audit_message: Option<&str>) {
        let (status, exit_code, message) = match result {
            Ok(()) if exit_code != 0 => (
                AuditEventStatus::Failure,
                exit_code,
                audit_message.map(str::to_owned),
            ),
            Ok(()) => (AuditEventStatus::Success, 0, None),
            Err(OrbitError::PolicyDenied(message) | OrbitError::CapabilityDenied(message)) => {
                (AuditEventStatus::Denied, 1, Some(message.clone()))
            }
            Err(error @ OrbitError::AuthorizationDenied(_)) => {
                (AuditEventStatus::Denied, 1, Some(error.to_string()))
            }
            Err(error @ OrbitError::DecisionRefused { code, .. }) if code.is_policy_denial() => {
                (AuditEventStatus::Denied, 1, Some(error.to_string()))
            }
            Err(error @ OrbitError::NoticeRefused { code, .. }) if code.is_policy_denial() => {
                (AuditEventStatus::Denied, 1, Some(error.to_string()))
            }
            Err(error) => (AuditEventStatus::Failure, 1, Some(error.to_string())),
        };
        self.status = status;
        self.exit_code = exit_code;
        self.error_message = message.as_deref().map(redact_sensitive_env_text);
    }

    /// Write the row, unless an `exec` or the tool entry already recorded the
    /// invocation on this thread.
    fn write(self) -> Result<(), AuditWriteFailure> {
        if take_tool_audit_recorded() {
            return Ok(());
        }
        let row = self.row();
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.runtime.record_audit_event(&row)
        })) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(AuditWriteFailure::Failed(error)),
            Err(_) => Err(AuditWriteFailure::Panicked),
        }
    }
}

/// Hand a succeeded command-line invocation's `AuditGuard` to the active
/// [`InvocationAuditScope`] on this thread, or settle it immediately when no
/// CLI settlement scope is open (for example, direct in-process `exec` calls).
pub(super) fn defer_or_finish(guard: AuditGuard, exit_code: i32) {
    let deferred = DEFERRED_INVOCATION_AUDIT.with(|slot| {
        let mut slot = slot.borrow_mut();
        match slot.as_mut() {
            Some(pending) if pending.is_none() => {
                *pending = Some(Box::new(DeferredAudit { guard, exit_code }));
                None
            }
            _ => Some(guard),
        }
    });
    if let Some(mut guard) = deferred {
        guard.finished(exit_code);
    }
}
