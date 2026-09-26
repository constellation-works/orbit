//! Runtime bootstrap and the two-root architecture (global + workspace).
//!
//! `OrbitRuntime` is initialized by locating two roots:
//! 1. **Global root** — `~/.orbit/`: houses global config,
//!    the audit SQLite database, skills, and globally-scoped resources.
//! 2. **Workspace root** — the nearest ancestor `.orbit/` directory from cwd:
//!    houses workspace-local tasks, knowledge, optional skill overrides, and runtime state.
//!
//! The `resolve` sub-module implements root discovery. The `builder` sub-module
//! wires together stores, policy, tool registry, and event bus into a complete
//! [`OrbitRuntime`], which `orbit_runtime` defines. The `engine`, `audit`,
//! `mutation`, and `tool_exec` sub-modules provide the high-level operations
//! exposed to command handlers; `plugin` and `workspace` own host plugins and
//! the workspace binding, catalog, and claim; `host_signal` probes host
//! lifecycle signals, such as a scheduled reboot, that hold new admissions.

mod activity_catalog;
pub(crate) mod assets;
pub mod audit;
pub(crate) mod authorization;
pub mod builder;
pub(crate) mod command_exec;
mod config_path;
pub(crate) mod cwd;
pub mod engine;
pub mod event_bus;
pub(crate) mod friction;
#[cfg(target_os = "linux")]
pub(crate) mod git_sandbox;
pub mod host_signal;
pub mod mutation;
mod orbit_runtime;
pub mod plugin;
pub(crate) mod recovery_authority;
mod resolve;
pub(crate) mod run_input;
pub(crate) mod task;
pub use task::StaleTaskReservation;
pub(crate) mod tool_exec;
mod worker_coordination;
pub mod workspace;

#[cfg(test)]
mod tests;

pub(crate) use config_path::{CONFIG_TOML_FILE, existing_config_file_path};
pub use orbit_runtime::{HostLifetime, OrbitRuntime, OrbitRuntimeRoots};
pub use workspace::binding::{WorkspaceRuntimeBinding, workspace_runtime_binding};

pub(crate) use resolve::{resolve_bootstrap_roots, resolve_initialize_roots};
// `pub` for the runtime-less `orbit migrate --dry-run` inspection that moved
// to `orbit-cmd` [ORB-10016].
pub use resolve::{
    ResolvedOrbitRoots, WorkspaceRootHint, resolve_bootstrap_roots_with_hint,
    resolve_initialize_roots_with_hint, try_resolve_initialized_roots_with_hint,
};
// `pub` for the runtime-less `orbit migrate --dry-run` inspection that moved
// to `orbit-cmd` [ORB-10016].
pub use resolve::{
    is_global_orbit_root, resolve_generation_root, resolve_global_root,
    resolve_process_generation_root, try_resolve_initialized_roots,
};
// `pub` for host task-store maintenance in `orbit-cmd`, which must recognize
// the one partition id no registry claims [ORB-12119].
pub use builder::UNBOUND_DATA_DIR_PARTITION_ID;
pub use run_input::managed_workspace_selector_from_env;
