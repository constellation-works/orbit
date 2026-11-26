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
//! # Four layers
//!
//! The kernel's state is four structs, each holding the one below and reached
//! by an accessor, never by `Deref`: [`Base`] (what every operation has),
//! [`Stored`] (the opened stores), [`Host`] (this machine as a place work
//! runs) and [`Run`] (the run this process carries). [`RuntimeKernel`] holds
//! the top one.
//!
//! The `base::path::resolve` sub-module implements root discovery. The `builder`
//! sub-module wires together stores, policy, tools, plugins, and event bus into a
//! complete [`RuntimeKernel`], while the four layers hold the runtime's
//! mechanisms: `base` (configuration, paths, actor, binding, event log,
//! authorization policy data, managed-run env parsing), `stored` (stores,
//! persistence, the task access chosen for the process's role, audit,
//! workspace catalog and claims, recovery authority), `host`
//! (PR forge, host signals, resource probes and forecasts, admission policy,
//! plugins, command/tool/script/Git-sandbox execution, embedded assets) and
//! `run` (worker coordination, engine helpers, step-recovery decisions, run
//! inputs, upgrade handover).

#![deny(clippy::print_stderr, clippy::print_stdout)]
// Legacy runtime command surfaces still need a focused documentation pass.
#![allow(missing_docs)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
#![cfg_attr(
    not(any(test, feature = "test-support")),
    deny(clippy::disallowed_methods)
)]

mod activity_catalog;
/// Host-local application authority audit hooks.
pub mod application_authority {
    pub use crate::application_consent::{finish_application_dispatch, take_application_audit};
}
mod application_consent;
mod application_port;
pub mod base;
pub mod builder;
pub mod cost;
pub mod friction;
pub mod host;
pub mod mutation;
mod orbit_runtime;
pub mod run;
pub mod stored;
pub mod task;

/// Compatibility re-exports for actor identity, admitted settings and stores.
pub mod context {
    pub use crate::base::actor::{
        ActorIdentity, ActorKind, resolve_write_actor_label, trusted_write_identity,
    };
    pub use crate::base::config::OrbitRuntimeSettings;
    pub use crate::stored::stores::OrbitStores;
}

pub use task::{InfraBlockedTask, StaleTaskReservation};

#[cfg(feature = "test-support")]
pub use host::resource::with_host_load_sampling_wait;
#[cfg(feature = "test-support")]
pub use run::worker_coordination::pipeline_poll_wait_override;
#[cfg(feature = "test-support")]
pub use run::worker_coordination::with_pipeline_poll_wait;
#[cfg(feature = "test-support")]
pub use stored::task_access::with_owner_read_retry_wait;

// Compatibility module re-exports so callers outside this crate keep their
// public paths until the wrapper is deleted.
pub use base::event as event_bus;
pub use base::path::paths;
pub use base::policy as authorization;
pub use host::exec::assets;
pub use host::exec::command_exec;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use host::exec::git_sandbox;
pub use host::exec::scripts;
pub use host::exec::tool_exec;
// Preserve the public forge path without exposing task runtime internals.
pub use host::forge as task_pr_forge;
pub use host::plugin;
pub use host::resource as host_resource;
pub use host::signal as host_signal;
pub use run::engine;
pub use run::recovery_decision;
pub use run::run_input;
pub use run::upgrade_handover;
pub use stored::audit;
pub use stored::recovery_authority;
pub use stored::workspace;

pub use application_port::ApplicationPort;
pub use base::Base;
pub use base::binding::{WorkspaceRuntimeBinding, workspace_runtime_binding};
pub use base::config::{CONFIG_TOML_FILE, existing_config_file_path};
pub use base::managed_env::managed_workspace_selector_from_env;
pub use base::role::Role;
pub use host::Host;
pub use orbit_runtime::{
    AdmissionCrewSettings, HostLifetime, OrbitRuntimeRoots, RuntimeConfigLoader, RuntimeKernel,
};
pub use run::Run;
pub use run::worker_coordination::{
    check_worker_host_input, check_worker_search_scope, is_coordination_tool,
};
pub use stored::Stored;
pub use stored::job_runs::JobRuns;

pub use base::path::resolve::{resolve_bootstrap_roots, resolve_initialize_roots};
// `pub` for the runtime-less `orbit migrate --dry-run` inspection that moved
// to `orbit-cmd` [ORB-10016].
pub use base::path::resolve::{
    ResolvedOrbitRoots, WorkspaceRootHint, initialized_explicit_workspace_root,
    resolve_bootstrap_roots_with_hint, resolve_initialize_roots_with_hint,
    try_resolve_initialized_cwd_roots_with_hint, try_resolve_initialized_roots_with_hint,
};
// `pub` for the runtime-less `orbit migrate --dry-run` inspection that moved
// to `orbit-cmd` [ORB-10016].
pub use base::path::resolve::{
    is_global_orbit_root, refuse_indirect_uninitialized_caller_orbit,
    resolve_existing_root_traversal, resolve_generation_root, resolve_global_root,
    resolve_process_generation_root, resolve_root_path_value, try_resolve_initialized_roots,
};
// `pub` for host task-store maintenance in `orbit-cmd`, which must recognize
// the one partition id no registry claims [ORB-12119].
pub use builder::UNBOUND_DATA_DIR_PARTITION_ID;
/// Guard this process's `gh` reads with the host's GitHub quota record. Only
/// the `orbit` binary calls it, so test and library processes stay unguarded.
pub use orbit_tools::github_quota::enable_host_guard as enable_github_quota_guard;

// A bare `cargo test` from a managed-run shell must match one from a clean shell.
#[cfg(test)]
orbit_common::isolate_test_process!();

#[cfg(test)]
mod tests;
