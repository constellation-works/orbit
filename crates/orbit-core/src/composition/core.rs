//! [`Core`]: the opaque handle a surface holds for one opened workspace.

use crate::OrbitRuntime;

/// One opened workspace, as a surface holds it.
///
/// A surface passes it to an app's `exec` (`orbit_core::app::<subject>::exec`)
/// and can do nothing else with it: it has no public field and no public
/// method. It holds the runtime privately until the runtime's layers are
/// built on demand.
pub struct Core {
    runtime: OrbitRuntime,
    entry: Entry,
}

/// How commands reach the `Core`, which decides what the chokepoint owes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Entry {
    /// A process that runs one command and exits. The chokepoint authorizes
    /// the command and writes its invocation audit row.
    CommandLine,
    /// A long-lived server answering many callers. The chokepoint authorizes
    /// each command; no invocation audit row is written for one today.
    Service,
    /// A tool handler inside the generic tool entry, which authorized and
    /// audited the call before the handler ran.
    Tool,
}

impl Core {
    /// The handle a tool handler passes to an app for a call the generic tool
    /// entry already authorized and audited.
    pub(crate) fn within_tool_entry(runtime: OrbitRuntime) -> Self {
        Self {
            runtime,
            entry: Entry::Tool,
        }
    }

    pub(crate) fn runtime(&self) -> &OrbitRuntime {
        &self.runtime
    }

    pub(crate) fn entry(&self) -> Entry {
        self.entry
    }
}

/// Open the `Core` of a process that runs one command and exits, over the
/// runtime composed for that command.
pub fn core_for_command(runtime: OrbitRuntime) -> Core {
    Core {
        runtime,
        entry: Entry::CommandLine,
    }
}

/// Open the `Core` of a long-lived server, over the runtime composed for one
/// of its workspaces.
pub fn core_for_service(runtime: OrbitRuntime) -> Core {
    Core {
        runtime,
        entry: Entry::Service,
    }
}
