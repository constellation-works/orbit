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
//! builds the builtin `proc.spawn` tool and other shell-invoking tools on top
//! of these primitives.
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
//! - [`EnvironmentMode`], [`StdinMode`] — environment and stdin control
//!
//! # Dependency direction
//! `orbit-types` → `orbit-exec` → orbit-tools

pub mod linux_landlock;
pub mod linux_sandbox;
pub mod macos_sandbox;
pub mod process;
pub mod result;
pub mod runner;
pub mod sandbox;
mod supervision;

pub use linux_landlock::{
    HOST_READ_ENV_VARS, LandlockGrant, LandlockPathGrant, LandlockProbeOutcome,
    LandlockReadBoundary, MINIMUM_LANDLOCK_ABI, grants_read, landlock_unavailable_message,
    linux_landlock_read_boundary, probe_landlock, spawn_under_linux_landlock,
};
pub use linux_sandbox::{
    BwrapProbeOutcome, LINUX_STABLE_BUILD_MOUNT, LINUX_STABLE_WORKSPACE_MOUNT, LinuxBwrapPlan,
    LinuxBwrapPostRunGuard, LinuxBwrapSpawnRequest, PreparedWriteGrants, UnsatisfiedWriteGrant,
    WriteAnchorKind, WriteGrant, bwrap_path, bwrap_program_for_audit, bwrap_unavailable_message,
    compile_linux_bwrap_argv, linux_bwrap_write_grant_diagnostic, linux_bwrap_write_grants,
    prepare_linux_bwrap_write_grants, probe_bwrap, spawn_under_linux_bwrap,
};
pub use macos_sandbox::{
    MacosLoginKeychainAccess, MacosSandboxSpawnRequest, claude_state_dir_from_env,
    compile_macos_sandbox_profile, grok_state_dir_from_env, macos_login_keychain_access,
    sandbox_exec_available, sandbox_exec_path, sandbox_exec_program_for_audit,
    sandbox_exec_unavailable_message, spawn_under_macos_sandbox,
};
pub use result::ExecutionResult;
pub use runner::{
    EnvironmentMode, ExecRequest, StdinMode, SupervisedOutcome, run_process,
    run_process_streaming_stdout, supervise_child,
};
pub use sandbox::{NoSandbox, Sandbox};
