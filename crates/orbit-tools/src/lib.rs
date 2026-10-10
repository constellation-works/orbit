#![deny(clippy::print_stderr, clippy::print_stdout)]
// Legacy tool-registry surfaces still need a focused documentation pass.
#![allow(missing_docs)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
//! Builtin tool registry providing the standard Orbit toolset for agents and jobs.
//!
//! Implements and registers all built-in tools that agents can invoke during
//! activity execution: git, GitHub, Orbit CLI, process, time, and
//! network tools. External (user-defined) tools are also supported via the registry.
//!
//! # Role
//! Depends on `orbit-exec` for process spawning and `orbit-common` for shared
//! types. Consumed by `orbit-engine`, `orbit-core`, and `orbit-mcp`, which
//! composes its workspace-scoped definitions with MCP discovery tools.
//!
//! # Key exports
//! - [`ToolRegistry`] — central registry; call `register_builtins()` to load all standard tools
//! - [`Tool`] trait — implement this to add a custom tool
//! - [`ToolContext`] — per-call context: cwd, allowed-tool allowlist, workspace root boundary
//! - [`require_str`] — helper to extract and validate string fields from tool input JSON
//! - [`check_exec_result`] — helper to turn a failed [`ExecutionResult`](orbit_types::tool::ExecutionResult) into an `OrbitError`
//!
//! # Registry contents
//! The builtin registry wires together the standard Orbit tool families:
//! git and GitHub helpers, Orbit task/job commands, process spawning,
//! network fetches, and time utilities. Each tool executes inside a
//! [`ToolContext`] that carries workspace boundaries, agent metadata,
//! process policies, and the narrow Orbit host surface used by Orbit builtins.
//!
//! # Dependency direction
//! orbit-common / orbit-exec / orbit-policy → `orbit-tools` → orbit-engine, orbit-core, orbit-mcp

pub(crate) mod builtin;
pub mod external;
pub mod github_cli;
mod mcp_annotations;
pub mod plugin;
mod registry;

mod context;
mod fs_audit;
mod host;
mod tool;

/// Default network operation timeout (15 s). Used for most GitHub API calls
/// and Orbit CLI commands where a quick response is expected.
pub(crate) const TIMEOUT_DEFAULT_MS: u64 = 15_000;

/// Slow operation timeout (30 s). Used for git network operations and PR creation,
/// which may involve larger payloads or slower remotes.
pub(crate) const TIMEOUT_SLOW_MS: u64 = 30_000;

/// Long operation timeout (60 s). Used for `gh pr checkout`, which clones or
/// fetches a branch and may transfer significant data over the network.
pub(crate) const TIMEOUT_LONG_MS: u64 = 60_000;

pub use builtin::orbit::pipeline::invoke::has_pipeline_child_admission;
pub use context::{
    ActivityBinding, DeterministicStepPrograms, ProcSpawnBudget, ReservationOwnerContext,
    ToolCaller, ToolContext, WitnessedProgramGrant,
};
pub use fs_audit::{FsAuditLogger, FsCallEvent, FsCallEventKind};
pub use host::{
    DrainOwnerTransport, OrbitBuiltinAction, OrbitTaskScope, OrbitToolHost, OwnerCoordinator,
    prepare_remote_task_artifact_put,
};
pub use registry::{ToolRegistry, canonical_builtin_mcp_tool_definitions};
pub(crate) use tool::upsert_env;
pub use tool::{Tool, ToolExecutionKind, check_exec_result, require_str};
