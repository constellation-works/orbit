//! The public tool surface: builtin definitions and MCP schemas, GitHub log
//! goldens, plugin loading, and proc-spawn and external-tool lockdown.
//!
//! One integration-test binary per area keeps link cost down; add a module
//! here rather than a new top-level `tests/*.rs` file
//! (`docs/design-patterns/test_strategy.md`).

// Integration fixtures unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod external_tool_lockdown;
mod mcp_definitions;
#[cfg(unix)]
mod plugin_environment;
mod plugin_loader;
mod proc_spawn_lockdown;
mod proc_spawn_timeout;
mod public_tool_surface;
