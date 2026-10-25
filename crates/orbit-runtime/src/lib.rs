#![deny(clippy::print_stderr, clippy::print_stdout)]
// Legacy runtime command surfaces still need a focused documentation pass.
#![allow(missing_docs)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
//! Orbit's runtime kernel: [`RuntimeKernel`], the mechanisms it owns, the
//! actor context, root paths and the cost summaries its engine records.
//!
//! # Role
//! An internal crate split out of `orbit-core` so an edit to application or
//! adapter code does not recompile the kernel. `orbit-core` owns the public
//! `OrbitRuntime`, a wrapper that owns a [`RuntimeKernel`] and dereferences to
//! it; application and adapter methods stay inherent on that wrapper. This
//! crate never depends on `orbit-core`, so the kernel cannot import use cases
//! or protocol adapters.
//!
//! # Two roots
//!
//! [`RuntimeKernel`] is initialized by locating two roots:
//! 1. **Global root** — `~/.orbit/`: houses global config,
//!    the audit SQLite database, skills, and globally-scoped resources.
//! 2. **Workspace root** — the nearest ancestor `.orbit/` directory from cwd:
//!    houses workspace-local tasks, knowledge, optional skill overrides, and runtime state.
//!
//! The `resolve` sub-module implements root discovery. The `builder` sub-module
//! wires together stores, policy, tool registry, and event bus into a complete
//! [`RuntimeKernel`], whose `orbit_runtime` module separates construction,
//! identity, host probes, persistence, configuration, and internal accessors.
//! The `engine`, `audit`,
//! `mutation`, and `tool_exec` sub-modules provide the high-level operations
//! exposed to command handlers; `plugin` and `workspace` own host plugins and
//! the workspace binding, catalog, and claim; `host_signal` probes host
//! lifecycle signals, such as a scheduled reboot, that hold new admissions;
//! `task::pr_forge` is the forge a task's terminal decision closes its
//! Orbit-authored PRs through.

// A bare `cargo test` from a managed-run shell must match one from a clean shell.
#[cfg(test)]
orbit_common::isolate_test_process!();

mod activity_catalog;
mod application_port;
pub mod assets;
pub mod audit;
pub mod authorization;
pub mod builder;
pub mod command_exec;
mod config_path;
pub mod context;
pub mod cost;
pub(crate) mod cwd;
pub mod engine;
pub mod event_bus;
pub mod friction;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod git_sandbox;
pub mod host_resource;
pub mod host_signal;
pub mod mutation;
mod orbit_runtime;
pub mod paths;
pub mod plugin;
pub mod recovery_authority;
pub mod recovery_decision;
mod resolve;
pub mod run_input;
pub mod task;
// Preserve the public forge path without exposing task runtime internals.
pub use task::pr_forge as task_pr_forge;
pub use task::{InfraBlockedTask, StaleTaskReservation};
pub mod tool_exec;
pub mod upgrade_handover;
mod worker_coordination;
#[cfg(feature = "test-support")]
pub(crate) use worker_coordination::host_load_sampling_wait_override;
#[cfg(feature = "test-support")]
pub use worker_coordination::pipeline_poll_wait_override;
#[cfg(feature = "test-support")]
pub use worker_coordination::{
    with_host_load_sampling_wait, with_owner_read_retry_wait, with_pipeline_poll_wait,
};
pub mod workspace;

#[cfg(test)]
mod tests;

pub use application_port::ApplicationPort;
pub use config_path::{CONFIG_TOML_FILE, existing_config_file_path};
pub use orbit_runtime::RuntimeConfigLoader;
pub use orbit_runtime::{HostLifetime, OrbitRuntimeRoots, RuntimeKernel};
pub use workspace::binding::{WorkspaceRuntimeBinding, workspace_runtime_binding};

pub use resolve::{resolve_bootstrap_roots, resolve_initialize_roots};
pub use worker_coordination::{
    check_worker_host_input, check_worker_search_scope, is_coordination_tool,
};
// `pub` for the runtime-less `orbit migrate --dry-run` inspection that moved
// to `orbit-cmd` [ORB-10016].
pub use resolve::{
    ResolvedOrbitRoots, WorkspaceRootHint, initialized_explicit_workspace_root,
    resolve_bootstrap_roots_with_hint, resolve_initialize_roots_with_hint,
    try_resolve_initialized_cwd_roots_with_hint, try_resolve_initialized_roots_with_hint,
};
// `pub` for the runtime-less `orbit migrate --dry-run` inspection that moved
// to `orbit-cmd` [ORB-10016].
pub use resolve::{
    is_global_orbit_root, refuse_indirect_uninitialized_caller_orbit, resolve_generation_root,
    resolve_global_root, resolve_process_generation_root, resolve_root_path_value,
    try_resolve_initialized_roots,
};
// `pub` for host task-store maintenance in `orbit-cmd`, which must recognize
// the one partition id no registry claims [ORB-12119].
pub use builder::UNBOUND_DATA_DIR_PARTITION_ID;
pub use run_input::managed_workspace_selector_from_env;
