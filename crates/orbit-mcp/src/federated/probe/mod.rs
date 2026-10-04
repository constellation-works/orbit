//! Live per-call probing and short-lived delivery to one configured destination.
//!
//! The mux answers `orbit.workspace.list` from what destinations say *now*, so
//! there is no health cache here and nothing is remembered between calls. A
//! routed workspace-scoped call opens one short-lived MCP session, confirms
//! the destination, and delivers that single `tools/call`. The client speaks
//! MCP over the same non-PTY SSH argv the v1 proxy uses.

mod contracts;
mod discovery;
mod local;
mod session;
mod ssh;

pub use contracts::{
    DEFAULT_PROBE_TIMEOUT, DEFAULT_ROUTED_DELIVERY_TIMEOUT, DestinationProbe, DestinationSnapshot,
    RoutedSession,
};
pub use local::{CompositeDestinationProbe, InProcessDestinationProbe};
pub use ssh::SshDestinationProbe;

#[cfg(test)]
pub(super) use session::DestinationSession;
