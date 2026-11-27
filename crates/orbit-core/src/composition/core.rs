//! [`Core`]: the opaque handle a surface holds for one opened workspace.

use crate::OrbitRuntime;
use crate::app::exec::{Caller, Session};

/// One opened workspace, as a surface holds it.
///
/// A surface passes it to an app's `exec` (`orbit_core::app::<subject>::exec`)
/// and can do nothing else with it: it has no public field and no public
/// method. It holds the runtime privately until the runtime's layers are
/// built on demand, and the caller the chokepoint admits and audits.
pub struct Core {
    runtime: OrbitRuntime,
    caller: Caller,
}

impl Core {
    /// The handle a tool handler passes to an app for a call the generic tool
    /// entry already authorized and audited.
    pub(crate) fn within_tool_entry(runtime: OrbitRuntime) -> Self {
        Self {
            runtime,
            caller: Caller::Tool,
        }
    }

    pub(crate) fn runtime(&self) -> &OrbitRuntime {
        &self.runtime
    }

    /// Who is asking. An app's `run` reads it where a flow needs to know.
    pub(crate) fn caller(&self) -> Caller {
        self.caller
    }
}

/// Open the `Core` of a process that runs one command and exits, over the
/// runtime composed for that command.
pub fn core_for_command(runtime: OrbitRuntime) -> Core {
    Core {
        runtime,
        caller: Caller::CommandLine,
    }
}

/// Open the `Core` of a long-lived server that answers for no session, over
/// the runtime composed for one of its workspaces. Its caller holds no grant.
pub fn core_for_service(runtime: OrbitRuntime) -> Core {
    core_for_session(runtime, Session::new(false))
}

/// Derive the `Core` of one request: the runtime a server composed for the
/// request's workspace, and the session the request arrived on. It opens
/// nothing; the runtime is the server's own handle.
pub fn core_for_session(runtime: OrbitRuntime, session: Session) -> Core {
    Core {
        runtime,
        caller: Caller::Session(session),
    }
}

/// Derive the `Core` of one request from the `Core` a server keeps for the
/// request's workspace: the same runtime handle, and the session the request
/// arrived on. It opens nothing.
pub fn core_for_request(workspace: &Core, session: Session) -> Core {
    core_for_session(workspace.runtime.clone(), session)
}
