//! The chokepoint every app's `exec` runs through: admit the command, with
//! its audit around it, and map the app's typed error into [`Error`].
//!
//! The stages are one file each: [`caller`] says who is asking, [`operation`]
//! what a command is and requires, [`admit`] decides, and [`audit`] chooses
//! and writes the row the caller is owed.

mod admit;

mod audit;

mod caller;

mod operation;

pub use admit::dashboard_authorize;
pub(crate) use admit::require;
pub(in crate::app) use audit::{AuditGuard, StreamAudit};
pub(crate) use audit::{DashboardRow, SessionAudit};
pub(crate) use caller::Caller;
pub use caller::Session;
pub use operation::DashboardOrigin;
pub(crate) use operation::Operation;

use serde_json::Value;

use crate::{Core, Error};

/// What an app supplies to the chokepoint: its types, the operation a command
/// performs, and how to run it.
pub(crate) trait App {
    type Command;
    type Outcome;
    type Error: Into<Error>;

    /// The registry operation `command` performs.
    fn operation(command: &Self::Command) -> Operation;

    /// Run `command`. The chokepoint has admitted it and started its audit.
    fn run(core: &Core, command: Self::Command) -> Result<Self::Outcome, Self::Error>;

    /// The exit code a command line reports for `outcome`. A command that
    /// ran but answers non-zero is audited as failed with that code.
    fn exit_code(_outcome: &Self::Outcome) -> i32 {
        0
    }

    /// The writer of the row a session is owed for `command`, where it is
    /// not a declared dashboard row.
    fn session_audit(_command: &Self::Command) -> Option<SessionAudit<Self::Outcome>> {
        None
    }

    /// What the declared dashboard row records about `result`, or `None`
    /// when an admitted outcome owes no row. `target` and `arguments` are
    /// the ones the operation declared.
    fn dashboard_row(
        result: &Result<Self::Outcome, Error>,
        _target: &str,
        arguments: Value,
    ) -> Option<DashboardRow> {
        Some(DashboardRow::of(result, arguments))
    }
}

/// Run one command of app `A`: admit it, with its audit around it.
pub(crate) fn run<A: App>(core: &Core, command: A::Command) -> Result<A::Outcome, Error> {
    let operation = A::operation(&command);
    // First, so a refusal is recorded.
    let audit = audit::start::<A>(core, &operation, &command);
    let result =
        admit::decide(core, &operation).and_then(|()| A::run(core, command).map_err(Into::into));
    audit.finish(core, &result);
    result
}

/// Decide what `command` requires of `session` before a workspace is opened
/// or discovered. `run` decides it again.
pub(crate) fn admit<A: App>(session: Session, command: &A::Command) -> Result<(), Error> {
    admit::early::<A>(session, command)
}

/// [`admit`], for an operation whose command needs an opened workspace to
/// build.
pub(crate) fn admit_operation(session: Session, operation: &Operation) -> Result<(), Error> {
    admit::early_operation(session, operation)
}

/// Authorize a streaming command and return an invocation audit guard that
/// stays open until the stream itself finishes.
///
/// If authorization or setup fails, the guard records that failure at once so
/// no row is lost; otherwise the caller holds the guard for the stream's life
/// and settles it with [`StreamAudit::finish`].
pub(in crate::app) fn start_stream<T>(
    core: &Core,
    operation: Operation,
    setup: impl FnOnce() -> Result<T, Error>,
) -> Result<(T, StreamAudit), Error> {
    if core.caller() == Caller::Tool {
        return setup().map(|value| (value, StreamAudit(None)));
    }
    let mut guard = (core.caller() == Caller::CommandLine && operation.invocation_audit)
        .then(|| AuditGuard::start(core, operation.clone()));
    match admit::decide(core, &operation).and_then(|()| setup()) {
        Ok(value) => Ok((value, StreamAudit(guard))),
        Err(error) => {
            if let Some(guard) = guard.as_mut() {
                guard.failed(&error);
            }
            Err(error)
        }
    }
}
