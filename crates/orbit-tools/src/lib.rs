#![deny(clippy::print_stderr, clippy::print_stdout)]
// Legacy tool-registry surfaces still need a focused documentation pass.
#![allow(missing_docs)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::disallowed_methods))]
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
mod family;
pub mod github_cli;
pub mod github_quota;
mod mcp;
pub mod plugin;
mod registry;
pub mod tool_suggestions;

mod artifact_materialize;
mod context;
mod fs_audit;
mod gh_request;
mod github_run_log_cache;
mod host;
mod program_policy;
mod task_guarded_selectors;
mod timeouts;
mod tool;

pub use artifact_materialize::prepare_remote_task_artifact_put;
pub use builtin::orbit::pipeline::invoke::has_pipeline_child_admission;
pub use context::{
    ActivityBinding, DeterministicStepPrograms, PROC_SPAWN_MAX_TIMEOUT_MINUTES, ProcSpawnBudget,
    ToolCaller, ToolContext, WitnessedProgramGrant,
};
pub use fs_audit::{FsAuditLogger, FsCallEvent, FsCallEventKind};
pub use github_run_log_cache::{GITHUB_RUN_LOG_CACHE_DIR, remove_legacy_github_run_log_cache};
pub use host::{
    DrainOwnerTransport, OrbitBuiltinAction, OrbitTaskScope, OrbitToolHost, OwnerCoordinator,
    ReservationOwnerContext,
};
pub use registry::{ToolRegistry, builtin_mcp_rules, canonical_builtin_mcp_tool_definitions};
pub(crate) use timeouts::{TIMEOUT_DEFAULT_MS, TIMEOUT_LONG_MS, TIMEOUT_SLOW_MS};
pub(crate) use tool::upsert_env;
pub use tool::{Tool, ToolExecutionKind, check_exec_result, require_str};
