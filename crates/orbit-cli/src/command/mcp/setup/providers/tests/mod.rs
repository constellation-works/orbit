#![allow(missing_docs)]

mod antigravity;
mod claude;
mod codex;
mod gemini;
mod grok;
mod simple_json;

// Content moved from inline #[cfg(test)] mod tests in providers/*.rs per ORB-00221.

use super::common::ServerLaunch;

/// The `orbit workspace init --mcp` bootstrap authority, with no workspace
/// binding, so authority assertions stay independent of the binding ones.
const OPERATOR_LAUNCH: ServerLaunch<'static> = ServerLaunch::local(true, None);

/// A launch bound to a registered workspace, as a generated config carries it.
const BOUND_LAUNCH: ServerLaunch<'static> = ServerLaunch::local(false, Some("ws_demo"));
