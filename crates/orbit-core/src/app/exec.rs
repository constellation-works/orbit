//! The chokepoint every app's `exec` runs through: authorize the command
//! against its operation's governance rows, audit it, run it, and map the
//! app's typed error into [`Error`].
//!
//! What a command is owed depends on how it reached the [`Core`]:
//!
//! - From a command-line process, the chokepoint authorizes the command and
//!   writes its invocation audit row, the row the CLI's own audit guard wrote
//!   before the command went through an app. It then marks the row as
//!   recorded, so that guard writes no second one.
//! - From a long-lived server, the chokepoint authorizes the command. No
//!   invocation row is written for one today.
//! - From a tool handler, the generic tool entry authorized and audited the
//!   call before the handler ran, so the chokepoint only runs it. This case
//!   ends when the tool entry itself is converted to call the apps.

use std::ffi::OsStr;
use std::time::Instant;

use orbit_common::OrbitError;
use orbit_common::governance::operation::OperationVerb;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_common::security::redaction::redact_sensitive_env_text;
use orbit_store::AuditEventInsertParams;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::ToolSessionContext;
use serde_json::Value;

use crate::composition::Entry;
use crate::runtime::tool_exec::CapabilityEnforcement;
use crate::{Core, Error};

/// What an app supplies to the chokepoint: its types, the operation a command
/// performs, and how to run it.
pub(crate) trait App {
    type Command;
    type Outcome;
    type Error: Into<Error>;

    /// The registry operation `command` performs.
    fn operation(command: &Self::Command) -> Operation;

    /// Run `command`. The chokepoint has authorized it and started its audit.
    fn run(core: &Core, command: Self::Command) -> Result<Self::Outcome, Self::Error>;
}

/// One command's identity in the operation registry: the names its governance
/// rows and its audit row are keyed on.
pub(crate) struct Operation {
    command: &'static str,
    subcommand: String,
    tool_name: &'static str,
    target_type: &'static str,
    target_id: Option<String>,
}

impl Operation {
    /// The operation of `verb`, called with `fields`. The audit names are the
    /// ones the CLI derives from the same spec: the noun, the CLI path joined
    /// by `-`, and the noun's target parameter.
    pub(crate) fn of<V: OperationVerb>(verb: V, fields: &Value) -> Self {
        let noun = V::noun();
        let spec = verb.spec();
        Self {
            command: noun.cli_name,
            subcommand: spec.cli_path.join("-"),
            tool_name: spec.tool_name,
            target_type: noun.audit_target_type,
            target_id: fields
                .get(noun.audit_target_param)
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        }
    }
}

/// Run one command of app `A`: authorize it, audit it, run it.
pub(crate) fn run<A: App>(core: &Core, command: A::Command) -> Result<A::Outcome, Error> {
    let operation = A::operation(&command);
    let mut guard = match core.entry() {
        Entry::Tool => return A::run(core, command).map_err(Into::into),
        Entry::CommandLine => Some(AuditGuard::start(core, &operation)),
        Entry::Service => None,
    };
    let result =
        authorize(core, &operation).and_then(|()| A::run(core, command).map_err(Into::into));
    if let Some(guard) = guard.as_mut() {
        guard.outcome(result.as_ref().err());
    }
    result
}

/// Decide `operation` against its governance rows: the command row a
/// command-line caller is held to, then the tool row every caller is held to.
/// An operation with no row passes. A refusal is recorded by the decision.
fn authorize(core: &Core, operation: &Operation) -> Result<(), Error> {
    let runtime = core.runtime();
    if core.entry() == Entry::CommandLine {
        runtime.authorize_command_operation(operation.command, &operation.subcommand)?;
    }
    // The session the tool entry decides the same call under.
    let mut session = ToolSessionContext::default();
    runtime.bind_worker_session(&mut session)?;
    runtime.authorize_tool_operation(
        operation.tool_name,
        &session,
        CapabilityEnforcement::Enforce,
    )
}

/// Writes one command's invocation audit row when dropped, so an early return
/// or a panic still leaves it. Until [`Self::outcome`] is called the row says
/// the command failed with exit code -1.
struct AuditGuard<'a> {
    core: &'a Core,
    operation: &'a Operation,
    execution_id: String,
    start: Instant,
    status: AuditEventStatus,
    exit_code: i32,
    error_message: Option<String>,
}

impl<'a> AuditGuard<'a> {
    fn start(core: &'a Core, operation: &'a Operation) -> Self {
        Self {
            core,
            operation,
            execution_id: audit_execution_id("exec"),
            start: Instant::now(),
            status: AuditEventStatus::Failure,
            exit_code: -1,
            error_message: None,
        }
    }

    /// Record how the command ended: `None` for success, or its error.
    fn outcome(&mut self, error: Option<&Error>) {
        let Some(error) = error else {
            self.status = AuditEventStatus::Success;
            self.exit_code = 0;
            self.error_message = None;
            return;
        };
        let denial = match error {
            OrbitError::PolicyDenied(message) | OrbitError::CapabilityDenied(message) => {
                Some(message.clone())
            }
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

    fn row(&self) -> AuditEventInsertParams {
        let runtime = self.core.runtime();
        let env = runtime.env();
        let env_text = |value: Option<&OsStr>| {
            value
                .and_then(OsStr::to_str)
                .filter(|text| !text.is_empty())
                .map(ToOwned::to_owned)
        };
        AuditEventInsertParams {
            execution_id: self.execution_id.clone(),
            command: self.operation.command.to_string(),
            subcommand: Some(self.operation.subcommand.clone()),
            tool_name: None,
            target_type: Some(self.operation.target_type.to_string()),
            target_id: self.operation.target_id.clone(),
            role: runtime.actor().audit_role().to_string(),
            status: self.status,
            exit_code: self.exit_code,
            duration_ms: self.start.elapsed().as_millis() as i64,
            working_directory: std::env::current_dir()
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string()),
            arguments_json: None,
            stdout_truncated: None,
            stderr_truncated: None,
            error_message: self.error_message.clone(),
            host: env
                .hostname()
                .and_then(OsStr::to_str)
                .map(ToOwned::to_owned),
            pid: std::process::id(),
            session_id: None,
            workspace_id: runtime
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
            job_run_id: env_text(env.orbit_run_id()),
            activity_id: env_text(env.orbit_activity_id()),
            step_index: env
                .orbit_step_index()
                .and_then(OsStr::to_str)
                .and_then(|text| text.parse().ok()),
        }
    }
}

impl Drop for AuditGuard<'_> {
    fn drop(&mut self) {
        let row = self.row();
        let written = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.core.runtime().record_audit_event(&row)
        }));
        // A row that was not written stays owed: the command-line entry's own
        // guard then writes it, and reports the failure if it fails again.
        if matches!(written, Ok(Ok(()))) {
            crate::adapter::command::mark_tool_audit_recorded();
        }
    }
}
