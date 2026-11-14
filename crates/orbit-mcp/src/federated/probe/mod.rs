//! Live per-call probing and delivery to one configured destination.
//!
//! The mux answers `orbit.workspace.list` from what destinations say *now*, so
//! there is no health cache here and no destination's answer is remembered
//! between calls. Listing opens its own SSH session per probe. A routed
//! workspace-scoped call takes an MCP session for that destination — a parked
//! one from [`RoutedSessionPool`] when its SSH argv and handshake binding match
//! and it passes a bounded `ping` health check, else a fresh one — runs live
//! discovery and confirms the destination on it, and delivers that single
//! `tools/call`. Only the transport and the remote `orbit mcp serve` process
//! are reused. The client speaks MCP over the same non-PTY SSH argv the v1
//! proxy uses.

mod contracts;
mod diagnostics;
mod discovery;
mod local;
mod pool;
mod session;
mod ssh;

pub use contracts::{
    DEFAULT_PROBE_TIMEOUT, DEFAULT_ROUTED_DELIVERY_TIMEOUT, DestinationProbe, DestinationSnapshot,
    RoutedSession,
};
pub use local::{CompositeDestinationProbe, InProcessDestinationProbe};
pub use pool::RoutedSessionPool;
pub use ssh::SshDestinationProbe;

#[cfg(test)]
mod tests;
