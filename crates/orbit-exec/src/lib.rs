#![deny(clippy::print_stderr, clippy::print_stdout)]
// Legacy process-execution surfaces still need a focused documentation pass.
#![allow(missing_docs)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
#![allow(
    rustdoc::broken_intra_doc_links,
    rustdoc::invalid_html_tags,
    rustdoc::private_intra_doc_links
)]
//! Process spawning, sandboxing, and timeout handling for Orbit tool execution.
//!
//! Provides the low-level primitives for launching child processes with
//! controlled environments, optional sandboxing, and configurable timeouts.
//! Results are captured and returned as [`ExecutionResult`] values.
//!
//! Sandbox selection in this crate can add an Orbit-controlled sandbox to a
//! subprocess; it cannot escape containment already applied to the parent
//! process. A child launched from a provider sandbox still inherits macOS
//! Seatbelt restrictions or, on Linux, the Bubblewrap mount namespace.
//!
//! # Role
//! Sits directly above `orbit-types` and is consumed by `orbit-tools`, which
//! builds the builtin `proc.spawn` tool, the plugin backend sandbox, and other
//! shell-invoking tools on top of these primitives.
//!
//! # Key exports
//! - [`run_process`] — primary entry point for spawning a subprocess
//! - [`supervise_child`] — supervise a child spawned through a sandbox wrapper
//! - [`ExecRequest`] — builder-style description of the process to run
//! - [`ExecutionResult`] — captured stdout/stderr, exit code, and duration
//! - [`Sandbox`] / [`NoSandbox`] — sandbox strategy trait and strategy that
//!   adds no additional Orbit sandbox
//! - [`spawn_under_linux_landlock`] — retained Linux read-confinement primitive;
//!   activity-scoped `proc.spawn` now inherits its enclosing worker sandbox
//! - [`spawn_under_linux_landlock_boundary`] — Linux read + write + TCP
//!   confinement to explicit granted roots, used by plugin backends
//! - [`InheritedFd`] — an open descriptor handed to the child at a fixed
//!   number, which is how a plugin backend receives its callback credential
//! - [`EnvironmentMode`], [`StdinMode`] — environment and stdin control
//! - [`run_build_phase`] — one install-time plugin build phase under the
//!   deny-by-default build profile, with [`probe_build_sandbox`] deciding
//!   whether this host can apply it and [`BUILD_FETCH_PHASE_SUPPORTED`]
//!   whether it runs a network `fetch` phase at all
//! - [`physical_with_missing_tail`] — the one resolution a granted path
//!   gets, shared by the layer that validates it and the layer that compiles
//!   the rule for it
//!
//! # Dependency direction
//! `orbit-types` → `orbit-exec` → orbit-tools

mod build_sandbox;
mod credential_paths;
mod linux_landlock;
mod linux_sandbox;
mod macos_sandbox;
mod path_identity;
mod process;
mod result;
mod runner;
mod sandbox;
mod supervision;

pub use build_sandbox::{
    BUILD_FETCH_PHASE_SUPPORTED, BuildLog, BuildPhaseEnd, BuildPhaseNetwork, BuildPhaseRequest,
    BuildSandboxProbe, BuildSandboxSpec, PLUGIN_BUILD_DIR_CAP_BYTES, PLUGIN_BUILD_FETCH_PORT,
    PLUGIN_BUILD_LOG_CAP_BYTES, compile_linux_build_argv, compile_macos_build_profile,
    probe_build_sandbox, run_build_phase,
};
pub use credential_paths::default_credential_read_denies;
pub use linux_landlock::{
    LandlockBoundary, LandlockPathGrant, NETWORK_LANDLOCK_ABI, WRITE_LANDLOCK_ABI, grants_read,
    linux_landlock_boundary_grants, linux_landlock_read_boundary, probe_landlock,
    spawn_under_linux_landlock, spawn_under_linux_landlock_boundary,
};
#[cfg(target_os = "linux")]
pub use linux_sandbox::probe_bwrap_fresh_for_user;
pub use linux_sandbox::{
    BwrapProbeOutcome, LINUX_STABLE_BUILD_MOUNT, LINUX_STABLE_WORKSPACE_MOUNT, LinuxBwrapMask,
    LinuxBwrapMountAuthority, LinuxBwrapPlan, LinuxBwrapPostRunGuard, LinuxBwrapSpawnRequest,
    UnsatisfiedWriteGrant, WriteAnchorKind, bwrap_path, bwrap_program_for_audit,
    compile_linux_bwrap_argv, compile_linux_bwrap_argv_with_authority, existing_glob_matches,
    linux_bwrap_write_grant_diagnostic, linux_bwrap_write_grants, prepare_linux_bwrap_write_grants,
    probe_bwrap, probe_bwrap_fresh, spawn_under_linux_bwrap,
};
pub use macos_sandbox::{
    MacosLoginKeychainAccess, MacosNetworkAccess, MacosSandboxSpawnRequest,
    append_macos_network_access, append_macos_read_boundary, append_macos_subpath_mask,
    claude_state_dir_from_env, compile_macos_sandbox_profile, macos_login_keychain_access,
    sandbox_exec_available, sandbox_exec_path, sandbox_exec_program_for_audit,
    sandbox_exec_unavailable_message, spawn_under_macos_sandbox,
};
pub use path_identity::{lexical_normalize, physical_with_missing_tail};
pub use process::{InheritedFd, spawn_with_inherited_fds};
pub use result::ExecutionResult;
pub use runner::{
    EnvironmentMode, ExecRequest, StdinMode, run_process, run_process_streaming_stdout,
    supervise_child, supervise_child_cancellable,
};
pub use sandbox::{NoSandbox, Sandbox};
