//! Entry points for one tool call: where it arrived from, the audit flag its
//! dispatch leaves on the thread, and the context used for recovery hints.

use std::cell::Cell;

use orbit_tools::ToolContext;
use orbit_types::tool::ToolSessionContext;

use super::super::audit::managed_run_context;
use super::super::callback::read_activity_tool_policy_from_env;

/// Resolve the activity grants relevant to recovery hints using the same
/// trusted-envelope rule as dispatch. Unmanaged MCP sessions ignore process policy.
pub fn tool_suggestion_context(
    entry_point: ToolEntryPoint,
    session_context: ToolSessionContext,
) -> ToolContext {
    let policy = if entry_point != ToolEntryPoint::Mcp || managed_run_context() {
        read_activity_tool_policy_from_env()
    } else {
        Default::default()
    };
    ToolContext {
        session_context,
        allowed_tools: policy.allowed_tools,
        tool_deny_policy: policy.deny_policy,
        ..Default::default()
    }
}

/// Where a tool invocation arrived from. Captured in the audit row so a single
/// audit table can attribute tool calls back to their origin (CLI vs MCP).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolEntryPoint {
    /// `orbit tool run ...` from the CLI binary.
    Cli,
    /// MCP `tools/call` over `orbit mcp serve`.
    Mcp,
}

impl ToolEntryPoint {
    /// Subcommand value written into the audit row. Distinct values let
    /// downstream queries filter MCP-originated calls without a schema change.
    pub fn audit_subcommand(self) -> &'static str {
        match self {
            ToolEntryPoint::Cli => "run",
            ToolEntryPoint::Mcp => "run-mcp",
        }
    }
}

#[cfg(unix)]
pub(super) fn broker_entry_point(entry_point: ToolEntryPoint) -> &'static str {
    match entry_point {
        ToolEntryPoint::Cli => "cli",
        ToolEntryPoint::Mcp => "mcp",
    }
}

thread_local! {
    static TOOL_AUDIT_RECORDED: Cell<bool> = const { Cell::new(false) };
}

/// Mark that the runtime has already persisted an audit row for the current
/// tool invocation on this thread. Higher layers (the CLI `AuditGuard`) call
/// [`take_tool_audit_recorded`] during their own teardown to suppress a
/// duplicate emission. The signal is per-thread and one-shot.
pub fn mark_tool_audit_recorded() {
    TOOL_AUDIT_RECORDED.with(|cell| cell.set(true));
}

/// Read and clear the per-thread tool-audit-recorded flag set by
/// [`mark_tool_audit_recorded`]. Returns `true` if a runtime-level audit row
/// was emitted on this thread since the last call.
pub fn take_tool_audit_recorded() -> bool {
    TOOL_AUDIT_RECORDED.with(|cell| cell.replace(false))
}
