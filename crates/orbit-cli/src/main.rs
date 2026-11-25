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

// The CLI binary prints genuine user-facing command output.
#![allow(clippy::print_stderr, clippy::print_stdout)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::disallowed_methods))]

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
    // The one read of the process environment, before logging starts.
    let env = orbit_common::Env::capture();
    // Before logging, generation pinning, or any store open. Test binaries
    // never enter `main`, so they keep unknown provenance.
    orbit_common::process::build::register_orbit_build(compiled_orbit_build());
    // Every `gh` read this process makes spends the host's shared GitHub
    // quota. Test binaries never enter `main`, so they run `gh` unguarded.
    orbit_core::runtime::enable_github_quota_guard();
    entry::run::run(env);
    // Statics are never dropped, so returning from `main` would not flush the
    // JSONL feed either.
    orbit_common::observability::logging::shutdown_jsonl_writer();
}

fn compiled_orbit_build() -> orbit_types::workflow::OrbitBuild {
    orbit_types::workflow::OrbitBuild {
        version: env!("CARGO_PKG_VERSION").into(),
        source_commit: match env!("ORBIT_BUILD_SOURCE_COMMIT") {
            "" => None,
            commit => Some(commit.into()),
        },
        dirty: env!("ORBIT_BUILD_DIRTY") != "false",
    }
}
