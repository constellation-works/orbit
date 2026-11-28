//! Which audit row a caller is owed, and the fields every row shares.
//!
//! - A command line is owed the invocation row ([`invocation`]). When the CLI
//!   entry scopes settlement with
//!   [`InvocationAuditScope`](crate::app::InvocationAuditScope), a successful
//!   `exec` defers that write until output rendering finishes (or unwinds),
//!   then marks the row as recorded so the scope writes no second one.
//! - A session is owed the row its command declares, or none: the
//!   `dashboard.operations` row of a governed dashboard write ([`dashboard`]),
//!   or the writer an app supplies (the task app's `task.mutation` row).
//! - A tool call is owed none: the generic tool entry audited it.

mod dashboard;

mod invocation;

use orbit_store::AuditEventInsertParams;
use orbit_types::telemetry::AuditEventStatus;

pub(crate) use dashboard::DashboardRow;
pub(in crate::app) use invocation::{AuditGuard, StreamAudit};

use self::dashboard::DashboardAudit;
use super::{App, Caller, Operation};
use crate::app::invocation::defer_or_finish;
use crate::{Core, Error};

/// The writer an app supplies for a session's command: it records the
/// command's one row once the outcome is known.
pub(crate) type SessionAudit<O> = Box<dyn FnOnce(&Core, &Result<O, Error>)>;

/// The audit one command is owed, started before it is admitted so a refusal
/// is recorded.
pub(super) enum Audit<A: App> {
    None,
    Invocation(Box<AuditGuard>),
    Dashboard(Box<DashboardAudit>),
    Session(SessionAudit<A::Outcome>),
}

impl<A: App> Audit<A> {
    /// Record how the command ended.
    pub(super) fn finish(self, core: &Core, result: &Result<A::Outcome, Error>) {
        match self {
            Self::None => {}
            Self::Invocation(mut guard) => match result {
                Ok(outcome) => defer_or_finish(*guard, A::exit_code(outcome)),
                Err(error) => guard.failed(error),
            },
            Self::Dashboard(audit) => audit.finish::<A>(core, result),
            Self::Session(write) => write(core, result),
        }
    }
}

/// Start the audit the caller of `core` is owed for `command`.
pub(super) fn start<A: App>(core: &Core, operation: &Operation, command: &A::Command) -> Audit<A> {
    match core.caller() {
        Caller::Tool => Audit::None,
        Caller::CommandLine if operation.invocation_audit => {
            Audit::Invocation(Box::new(AuditGuard::start(core, operation.clone())))
        }
        Caller::CommandLine => Audit::None,
        Caller::Session(session) => match &operation.dashboard_row {
            Some(row) => Audit::Dashboard(Box::new(DashboardAudit::start(session, row.clone()))),
            None => A::session_audit(command).map_or(Audit::None, Audit::Session),
        },
    }
}

/// A row for `command` with the fields every row shares: this process, and
/// no attribution a writer has not named.
fn unattributed_row(execution_id: String, command: &str) -> AuditEventInsertParams {
    AuditEventInsertParams {
        execution_id,
        command: command.to_string(),
        subcommand: None,
        tool_name: None,
        target_type: None,
        target_id: None,
        role: String::new(),
        status: AuditEventStatus::Failure,
        exit_code: -1,
        duration_ms: 0,
        working_directory: String::new(),
        arguments_json: None,
        stdout_truncated: None,
        stderr_truncated: None,
        error_message: None,
        host: None,
        pid: std::process::id(),
        session_id: None,
        workspace_id: None,
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
        job_run_id: None,
        activity_id: None,
        step_index: None,
    }
}
