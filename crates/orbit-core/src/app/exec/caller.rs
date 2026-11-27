//! [`Caller`]: who is asking, and how that resolves to the capabilities the
//! one decision function reads.

use orbit_common::governance::authorization::{CallerCapabilities, CallerEnvelope};
use orbit_types::tool::{McpCapability, ToolSessionContext};

use crate::Core;

/// An authenticated server session and the grants it holds.
///
/// A server derives a `Core` for a request from its workspace runtime and
/// the request's session, so no command carries the session itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Session {
    operator: bool,
}

impl Session {
    /// A session that holds operator capability when `operator` is set, and
    /// no grant otherwise.
    pub fn new(operator: bool) -> Self {
        Self { operator }
    }

    /// The session's own grants. Session state only: the server's
    /// stdin/stderr, `ORBIT_OPERATOR` and agent envelope never grant here, so
    /// a `--no-operator` server on a pty (an SSH `-tt` tunnel, or a
    /// terminal-started `web serve`) stays read-only, and a granted write is
    /// audited as the session [ORB-23047].
    pub(super) fn capabilities(self) -> CallerCapabilities {
        let mut session = ToolSessionContext::default();
        if self.operator {
            session.effective_capabilities.insert(McpCapability::Agent);
            session
                .effective_capabilities
                .insert(McpCapability::Operator);
        }
        CallerCapabilities::resolve(&CallerEnvelope::dashboard_session(&session))
    }
}

/// How a command reached the `Core`, which decides what the chokepoint owes
/// it: how it is admitted and which audit row it is owed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Caller {
    /// A process that runs one command and exits. It is admitted against the
    /// process environment and owed the invocation audit row.
    CommandLine,
    /// A long-lived server answering for a session. It is admitted against
    /// the session's grants and owed the row its command declares, or none.
    Session(Session),
    /// A tool handler inside the generic tool entry, which authorized and
    /// audited the call before the handler ran.
    Tool,
}

impl Caller {
    /// The capabilities a governed requirement is decided against: the
    /// session's own for a session, the process environment's otherwise.
    pub(super) fn capabilities(self, core: &Core) -> CallerCapabilities {
        match self {
            Self::Session(session) => session.capabilities(),
            Self::CommandLine | Self::Tool => {
                CallerCapabilities::resolve(&CallerEnvelope::from_process_env(
                    core.runtime().env(),
                    &ToolSessionContext::default(),
                ))
            }
        }
    }
}
