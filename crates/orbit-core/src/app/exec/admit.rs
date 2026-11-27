//! The one place a command is authorized.
//!
//! Every path ends in `orbit_common`'s pure decision function. The callers
//! differ in how their capabilities resolve ([`Caller`]) and in the type of
//! the refusal: a session gets the typed [`AuthorizationDenial`], which the
//! dashboard renders as its 403 body; any other caller keeps
//! `CapabilityDenied(message)`.

use orbit_common::OrbitError;
use orbit_common::governance::authorization::{
    AuthorizationDenial, CallerCapabilities, CallerEnvelope, GovernedOperation, authorize,
};
use orbit_types::tool::{McpCapability, ToolSessionContext};

use super::operation::Requirement;
use super::{App, Caller, Operation, Session};
use crate::runtime::tool_exec::CapabilityEnforcement;
use crate::{Core, Error};

/// Decide `operation` for the caller of `core`: the command row a
/// command-line caller is held to, then each declared requirement in order,
/// then the tool row. An operation with no row passes. A refused command or
/// tool row is recorded by the decision; a session's refusal is recorded by
/// the audit stage.
///
/// A tool call was decided by the generic tool entry, so only its declared
/// governed requirements are decided here.
pub(super) fn decide(core: &Core, operation: &Operation) -> Result<(), Error> {
    let runtime = core.runtime();
    let caller = core.caller();
    if caller == Caller::CommandLine && operation.command_row {
        runtime.authorize_command_operation(operation.command, &operation.subcommand)?;
    }
    for requirement in &operation.requirements {
        match requirement {
            Requirement::Governed(governed) => require(core, governed)?,
            Requirement::NativeOperator(_) if caller == Caller::Tool => {}
            Requirement::NativeOperator(refusal) => {
                let process =
                    CallerEnvelope::from_process_env(runtime.env(), &ToolSessionContext::default());
                if process.agent_declared
                    || !CallerCapabilities::resolve(&process)
                        .grants()
                        .contains(&McpCapability::Operator)
                {
                    return Err(OrbitError::CapabilityRefused((*refusal).into()));
                }
            }
        }
    }
    let (Some(tool_name), false) = (operation.tool_name, caller == Caller::Tool) else {
        return Ok(());
    };
    // The session the tool entry decides the same call under.
    let mut session = ToolSessionContext::default();
    runtime.bind_worker_session(&mut session)?;
    runtime.authorize_tool_operation(tool_name, &session, CapabilityEnforcement::Enforce)
}

/// Decide one governed requirement for the caller of `core`. A flow calls
/// this for a requirement that depends on a record it has just read.
pub(crate) fn require(core: &Core, governed: &'static GovernedOperation) -> Result<(), Error> {
    match core.caller() {
        Caller::Session(session) => session_requires(session, governed),
        caller @ (Caller::CommandLine | Caller::Tool) => {
            authorize(governed, &caller.capabilities(core))
                .map_err(|denial| OrbitError::CapabilityDenied(denial.to_string()))
        }
    }
}

/// Decide the governed requirements of `command` for `session` without a
/// `Core`, so a surface can refuse before it opens or discovers a workspace.
/// The decision needs only the session and the operation and records
/// nothing. `exec` decides the same requirements again.
pub(super) fn early<A: App>(session: Session, command: &A::Command) -> Result<(), Error> {
    early_operation(session, &A::operation(command))
}

/// [`early`], for an operation whose command needs an opened workspace to
/// build.
pub(super) fn early_operation(session: Session, operation: &Operation) -> Result<(), Error> {
    operation
        .requirements
        .iter()
        .try_for_each(|requirement| match requirement {
            Requirement::Governed(governed) => session_requires(session, governed),
            Requirement::NativeOperator(_) => Ok(()),
        })
}

fn session_requires(session: Session, governed: &'static GovernedOperation) -> Result<(), Error> {
    authorize(governed, &session.capabilities())
        .map_err(|denial| OrbitError::AuthorizationDenied(Box::new(denial)))
}

/// Decide a dashboard operation using only its authenticated session grants.
///
/// Exported for the dashboard handlers that still hold the runtime and
/// authorize themselves; a handler that calls an app's `exec` does not need
/// it.
pub fn dashboard_authorize(
    operation: &'static GovernedOperation,
    operator_session: bool,
) -> Result<CallerCapabilities, AuthorizationDenial> {
    let caller = Session::new(operator_session).capabilities();
    authorize(operation, &caller)?;
    Ok(caller)
}
