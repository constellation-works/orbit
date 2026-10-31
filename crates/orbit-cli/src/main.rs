// Legacy CLI binary surfaces still need a focused documentation pass.
#![allow(missing_docs)]
// The CLI binary prints genuine user-facing command output.
#![allow(clippy::print_stderr, clippy::print_stdout)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

//! CLI entry point for Orbit: command parsing, dispatch, and output formatting.
//!
//! Parses command-line arguments with `clap`, initializes the [`OrbitRuntime`](orbit_core::OrbitRuntime),
//! dispatches to the appropriate command handler, and formats results as JSON
//! or human-readable table output. Wraps every command in an audit middleware
//! that records success, failure, or policy-denial events.
//!
//! # Role
//! The outermost crate in the dependency graph. Depends on `orbit-core` and
//! `orbit-types`. All other crates are consumed transitively via `orbit-core`.
//! This binary is the `orbit` executable.
//!
//! # Key responsibilities
//! - Parse the top-level CLI surface and route subcommands to their handlers
//! - Bootstrap the runtime, including optional `--root` data-dir overrides and
//!   `--workspace` selectors (registered name, logical id, or checkout path)
//! - Emit machine-readable JSON or human-readable table output
//! - Wrap command execution in audit logging so human and agent actions are recorded
//!
//! # Dependency direction
//! orbit-core, orbit-types → `orbit-cli` (binary crate, no dependents)

mod audit_middleware;
mod command;
mod entry;
mod help;
mod output;
mod parse;
mod plugin_cli;
mod root_check;
mod usage_error;

#[cfg(test)]
mod tests;

fn main() {
    entry::run::run();
    // Statics are never dropped, so returning from `main` would not flush the
    // JSONL feed either.
    orbit_common::observability::logging::shutdown_jsonl_writer();
}
