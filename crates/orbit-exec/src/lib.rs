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
//! - [`spawn_under_linux_landlock`] — Linux read confinement applied to the
//!   child itself, used by activity-scoped `proc.spawn`
//! - [`spawn_under_linux_landlock_boundary`] — Linux read + write + TCP
//!   confinement to explicit granted roots, used by plugin backends
//! - [`InheritedFd`] — an open descriptor handed to the child at a fixed
//!   number, which is how a plugin backend receives its callback credential
//! - [`EnvironmentMode`], [`StdinMode`] — environment and stdin control
//! - [`physical_with_missing_tail`] / [`create_write_root`] — the one
//!   resolution a granted path gets, shared by the layer that validates it
//!   and the layer that compiles the rule for it
//!
//! # Dependency direction
//! `orbit-types` → `orbit-exec` → orbit-tools

pub mod linux_landlock;
pub mod linux_sandbox;
pub mod macos_sandbox;
pub mod path_identity;
pub mod process;
pub mod result;
pub mod runner;
pub mod sandbox;
mod supervision;

pub use linux_landlock::{
    HOST_READ_ENV_VARS, LandlockBoundary, LandlockGrant, LandlockPathGrant, LandlockProbeOutcome,
    LandlockReadBoundary, MINIMUM_LANDLOCK_ABI, NETWORK_LANDLOCK_ABI, grants_read,
    landlock_unavailable_message, linux_landlock_boundary_grants, linux_landlock_read_boundary,
    probe_landlock, spawn_under_linux_landlock, spawn_under_linux_landlock_boundary,
};
pub use linux_sandbox::{
    BwrapProbeOutcome, LINUX_STABLE_BUILD_MOUNT, LINUX_STABLE_WORKSPACE_MOUNT,
    LinuxBwrapMountAuthority, LinuxBwrapMountEvidence, LinuxBwrapPlan, LinuxBwrapPostRunGuard,
    LinuxBwrapSpawnRequest, PreparedWriteGrants, UnsatisfiedWriteGrant, WriteAnchorKind,
    WriteGrant, bwrap_path, bwrap_program_for_audit, bwrap_unavailable_message,
    compile_linux_bwrap_argv, compile_linux_bwrap_argv_with_authority,
    linux_bwrap_write_grant_diagnostic, linux_bwrap_write_grants, prepare_linux_bwrap_write_grants,
    probe_bwrap, spawn_under_linux_bwrap,
};
pub use macos_sandbox::{
    MacosLoginKeychainAccess, MacosNetworkAccess, MacosSandboxSpawnRequest,
    append_macos_network_access, append_macos_read_boundary, claude_state_dir_from_env,
    compile_macos_sandbox_profile, grok_state_dir_from_env, macos_login_keychain_access,
    sandbox_exec_available, sandbox_exec_path, sandbox_exec_program_for_audit,
    sandbox_exec_unavailable_message, spawn_under_macos_sandbox,
};
pub use path_identity::{create_write_root, lexical_normalize, physical_with_missing_tail};
pub use process::{InheritedFd, spawn_with_inherited_fds};
pub use result::ExecutionResult;
pub use runner::{
    EnvironmentMode, ExecRequest, StdinMode, SupervisedOutcome, run_process,
    run_process_streaming_stdout, supervise_child,
};
pub use sandbox::{NoSandbox, Sandbox};
