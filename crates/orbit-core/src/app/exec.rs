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
//!   invocation row is written for one today; a task write records the
//!   owner-side mutation row the dashboard wrote itself before.
//! - From a tool handler, the generic tool entry authorized and audited the
//!   call before the handler ran, so the chokepoint only runs it. This case
//!   ends when the tool entry itself is converted to call the apps.

use std::ffi::OsStr;
use std::time::Instant;

use orbit_common::OrbitError;
use orbit_common::governance::authorization::{
    AuthorizationDenial, CallerCapabilities, CallerEnvelope, GovernedOperation,
    authorize as decide_dashboard,
};
use orbit_common::governance::operation::OperationVerb;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_common::security::redaction::redact_sensitive_env_text;
use orbit_store::AuditEventInsertParams;
use orbit_types::task::TaskWriteSurface;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde_json::Value;

use crate::application::task::{TaskMutationAudit, audit_status_for_task_mutation_error};
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

    /// The exit code a command line reports for `outcome`. A command that
    /// ran but answers non-zero is audited as failed with that code.
    fn exit_code(_outcome: &Self::Outcome) -> i32 {
        0
    }

    /// The mutation row a server owes for `command`, if it is a write the
    /// dashboard records.
    fn mutation_audit(_command: &Self::Command) -> Option<MutationAudit> {
        None
    }

    /// The record `outcome` wrote, when the command could not name it.
    fn written_id(_outcome: &Self::Outcome) -> Option<String> {
        None
    }
}

/// What a server records about one write: the `task.mutation` row's verb,
/// target, caller and detail. The role is the given one, or the one resolved
/// for `model`.
pub(crate) struct MutationAudit {
    pub(crate) verb: &'static str,
    pub(crate) target_id: Option<String>,
    pub(crate) caller: Option<String>,
    pub(crate) role: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) detail: Value,
}

/// One command's identity in the operation registry: the names its governance
/// rows and its audit row are keyed on.
#[derive(Clone)]
pub(crate) struct Operation {
    command: &'static str,
    subcommand: String,
    /// The tool whose governance row this chokepoint decides, if any.
    tool_name: Option<&'static str>,
    target_type: Option<&'static str>,
    target_id: Option<String>,
    job_run_id: Option<String>,
    native_operator_refusal: Option<&'static str>,
    audit_role: Option<&'static str>,
    /// Whether a command-line caller is held to the command's governance row.
    command_row: bool,
}

impl Operation {
    /// The operation of `verb`, called with `fields`. The audit names are the
    /// ones the CLI derives from the same spec: the noun, the CLI path joined
    /// by `-`, and the noun's target parameter.
    pub(crate) fn of<V: OperationVerb>(verb: V, fields: &Value) -> Self {
        let noun = V::noun();
        let spec = verb.spec();
        Self {
            native_operator_refusal: None,
            audit_role: None,
            job_run_id: None,
            command_row: true,
            command: noun.cli_name,
            subcommand: spec.cli_path.join("-"),
            tool_name: Some(spec.tool_name),
            target_type: Some(noun.audit_target_type),
            target_id: fields
                .get(noun.audit_target_param)
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        }
    }

    /// A command with no verb in the operation registry: it has no tool row
    /// and no target until [`Self::target`] names one.
    pub(crate) fn command(command: &'static str, subcommand: impl Into<String>) -> Self {
        Self {
            command,
            native_operator_refusal: None,
            audit_role: None,
            job_run_id: None,
            command_row: true,
            subcommand: subcommand.into(),
            tool_name: None,
            target_type: None,
            target_id: None,
        }
    }

    pub(crate) fn require_native_operator(mut self, refusal: &'static str) -> Self {
        self.native_operator_refusal = Some(refusal);
        self
    }

    /// Attribute an invocation to its named source run.
    pub(crate) fn job_run(mut self, id: &str) -> Self {
        self.job_run_id = Some(id.to_owned());
        self
    }

    /// Name the audit row's target where the noun's own is not the one.
    pub(crate) fn target(
        mut self,
        target_type: Option<&'static str>,
        target_id: Option<String>,
    ) -> Self {
        self.target_type = target_type;
        self.target_id = target_id;
        self
    }

    /// Preserve the command's explicit audit attribution.
    pub(crate) fn audit_role(mut self, role: &'static str) -> Self {
        self.audit_role = Some(role);
        self
    }

    /// Hold no caller to the command's governance row: the invocation is the
    /// non-destructive form of a command whose destruction is flag-gated.
    pub(crate) fn without_command_row(mut self) -> Self {
        self.command_row = false;
        self
    }

    /// Decide no tool row here: the command either runs through the generic
    /// tool entry, which decides the row, or never was a tool call. Deciding
    /// it here too would record one decision twice.
    pub(crate) fn without_tool_row(mut self) -> Self {
        self.tool_name = None;
        self
    }
}

/// Run one command of app `A`: authorize it, audit it, run it.
pub(crate) fn run<A: App>(core: &Core, command: A::Command) -> Result<A::Outcome, Error> {
    let operation = A::operation(&command);
    let admitted = |command| {
        authorize(core, &operation).and_then(|()| A::run(core, command).map_err(Into::into))
    };
    match core.entry() {
        Entry::Tool => A::run(core, command).map_err(Into::into),
        Entry::CommandLine => {
            let mut guard = AuditGuard::start(core, &operation);
            let result = admitted(command);
            match &result {
                Ok(outcome) => guard.finished(A::exit_code(outcome)),
                Err(error) => guard.failed(error),
            }
            result
        }
        Entry::Service => {
            let Some(audit) = A::mutation_audit(&command) else {
                return admitted(command);
            };
            let started = Instant::now();
            let result = admitted(command);
            record_mutation(core, audit, started, result.as_ref().map(A::written_id));
            result
        }
    }
}

/// Write the `task.mutation` row of one server write. A write that named no
/// target is recorded against the record it wrote.
fn record_mutation(
    core: &Core,
    audit: MutationAudit,
    started: Instant,
    result: Result<Option<String>, &Error>,
) {
    let runtime = core.runtime();
    let duration_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
    let role = audit
        .role
        .unwrap_or_else(|| runtime.resolve_task_mutation_audit_role(None, audit.model.as_deref()));
    let (target_id, status, error_message) = match result {
        Ok(written) => (written.or(audit.target_id), AuditEventStatus::Success, None),
        Err(error) => (
            audit.target_id,
            audit_status_for_task_mutation_error(error),
            Some(error.to_string()),
        ),
    };
    runtime.record_task_mutation_audit(TaskMutationAudit {
        verb: audit.verb,
        target_id: target_id.as_deref(),
        role: &role,
        surface: TaskWriteSurface::Web,
        caller: audit.caller.as_deref(),
        status,
        duration_ms,
        error_message,
        detail: audit.detail,
        session_context: None,
    });
}

/// Decide `operation` against its governance rows: the command row a
/// command-line caller is held to, then the tool row every caller is held to.
/// An operation with no row passes. A refusal is recorded by the decision.
fn authorize(core: &Core, operation: &Operation) -> Result<(), Error> {
    let runtime = core.runtime();
    if core.entry() == Entry::CommandLine && operation.command_row {
        runtime.authorize_command_operation(operation.command, &operation.subcommand)?;
    }
    if let Some(refusal) = operation.native_operator_refusal {
        let caller =
            CallerEnvelope::from_process_env(runtime.env(), &ToolSessionContext::default());
        if caller.agent_declared
            || !CallerCapabilities::resolve(&caller)
                .grants()
                .contains(&McpCapability::Operator)
        {
            return Err(OrbitError::CapabilityRefused(refusal.into()));
        }
    }
    let Some(tool_name) = operation.tool_name else {
        return Ok(());
    };
    // The session the tool entry decides the same call under.
    let mut session = ToolSessionContext::default();
    runtime.bind_worker_session(&mut session)?;
    runtime.authorize_tool_operation(tool_name, &session, CapabilityEnforcement::Enforce)
}

/// Writes one command's invocation audit row when dropped, so an early return
/// or a panic still leaves it. Until [`Self::finished`] is called the row says
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

    /// Record that the command ran and answers `exit_code`.
    fn finished(&mut self, exit_code: i32) {
        self.status = match exit_code {
            0 => AuditEventStatus::Success,
            _ => AuditEventStatus::Failure,
        };
        self.exit_code = exit_code;
        self.error_message = None;
    }

    /// Record the error the command ended with.
    fn failed(&mut self, error: &Error) {
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

/// Decide a dashboard operation using only its authenticated session grants.
pub fn dashboard_authorize(
    operation: &'static GovernedOperation,
    operator_session: bool,
) -> Result<CallerCapabilities, AuthorizationDenial> {
    let caller = dashboard_caller(operator_session);
    decide_dashboard(operation, &caller)?;
    Ok(caller)
}

/// Decide the requested controls against the same authenticated session.
/// The first refusal is the one reported by the dashboard.
pub(super) fn dashboard_controls<const N: usize>(
    operator: bool,
    controls: [(bool, &'static GovernedOperation); N],
) -> Result<(), AuthorizationDenial> {
    let caller = dashboard_caller(operator);
    controls
        .into_iter()
        .filter(|(requested, _)| *requested)
        .try_for_each(|(_, operation)| decide_dashboard(operation, &caller))
}

fn dashboard_caller(operator_session: bool) -> CallerCapabilities {
    let mut session = ToolSessionContext::default();
    if operator_session {
        session.effective_capabilities.insert(McpCapability::Agent);
        session
            .effective_capabilities
            .insert(McpCapability::Operator);
    }
    // Session state only: the server's own stdin/stderr, `ORBIT_OPERATOR` and
    // agent envelope never grant here, so a `--no-operator` server on a pty
    // (an SSH `-tt` tunnel, or a terminal-started `web serve`) stays
    // read-only, and a granted write is audited as the session [ORB-23047].
    CallerCapabilities::resolve(&CallerEnvelope::dashboard_session(&session))
}

/// The arguments and authenticated session of a governed dashboard write.
pub(super) struct DashboardOperation<'a> {
    pub(super) operation: &'static GovernedOperation,
    pub(super) target: &'a str,
    pub(super) arguments: Value,
    pub(super) operator_session: bool,
    pub(super) host: Option<String>,
}

/// Run a dashboard operation and record its one dashboard audit row.
/// A missing target carries the same message as the dashboard response.
pub(super) fn run_dashboard<T>(
    core: &Core,
    operation: DashboardOperation<'_>,
    run: impl FnOnce() -> Result<T, Error>,
    failure: impl FnOnce(&T) -> Option<String>,
) -> Result<Result<T, AuthorizationDenial>, Error> {
    let started = Instant::now();
    let runtime = core.runtime();
    let workspace = runtime
        .workspace_id()
        .unwrap_or_else(|_| runtime.shared_root().display().to_string());
    let (result, status, provenance, capabilities, error_message) =
        match dashboard_authorize(operation.operation, operation.operator_session) {
            Ok(caller) => {
                let result = run();
                let message = match &result {
                    Ok(value) => failure(value),
                    Err(error) => Some(error.to_string()),
                };
                (
                    result.map(Ok),
                    if message.is_some() {
                        AuditEventStatus::Failure
                    } else {
                        AuditEventStatus::Success
                    },
                    caller.provenance(),
                    caller.grants().clone(),
                    message,
                )
            }
            Err(denial) => {
                let provenance = denial.provenance;
                let message = denial.to_string();
                (
                    Ok(Err(denial)),
                    AuditEventStatus::Denied,
                    provenance,
                    Default::default(),
                    Some(message),
                )
            }
        };
    let row = AuditEventInsertParams {
        execution_id: audit_execution_id("dashboard-operations"),
        command: "dashboard.operations".to_string(),
        subcommand: Some(provenance.to_string()),
        tool_name: None,
        target_type: Some(operation.operation.id.to_string()),
        target_id: Some(operation.target.to_string()),
        role: "operator".to_string(),
        status,
        exit_code: i32::from(status != AuditEventStatus::Success),
        duration_ms: i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX),
        working_directory: runtime
            .workspace_runtime_binding()
            .map(|binding| binding.repo_root.display().to_string())
            .unwrap_or_else(|| runtime.shared_root().display().to_string()),
        arguments_json: serde_json::to_string(&operation.arguments).ok(),
        stdout_truncated: None,
        stderr_truncated: None,
        error_message,
        host: operation.host,
        pid: std::process::id(),
        session_id: None,
        workspace_id: Some(workspace),
        caller_machine_id: None,
        caller_machine_name: Some(String::new()),
        process_machine_id: None,
        process_machine_name: Some(String::new()),
        transport: None,
        effective_capabilities: capabilities,
        origin_session_id: None,
        mcp_call_id: None,
        lease_id: None,
        task_id: None,
        job_run_id: None,
        activity_id: None,
        step_index: None,
    };
    if let Err(error) = runtime.record_audit_event(&row) {
        tracing::error!(operation = operation.operation.id, target = operation.target, %error,
            "failed to persist dashboard operation audit");
    }
    result
}
