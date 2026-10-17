//! Platform sandbox enforcement: Linux Landlock and bwrap, and macOS
//! `sandbox-exec`. Kernel modules compile only on their platform; apply-probe
//! classification and guard-output fixtures run on every platform.
//!
//! One integration-test binary per area keeps link cost down; add a module
//! here rather than a new top-level `tests/*.rs` file
//! (`docs/design-patterns/test_strategy.md`).

// Integration fixtures unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used, missing_docs)]

#[cfg(target_os = "linux")]
#[path = "../support/git.rs"]
mod git_test_support;

mod linux_landlock;
mod linux_runtime_ipc;
mod linux_sandbox;
mod macos_compile;
mod macos_sandbox;

mod apply_probe;

mod audit_support;
mod build_sandbox;
mod linux_credentials;
mod linux_masks;
mod macos_audit;
mod macos_audit_support;
mod macos_provider;
mod macos_spawn;
mod path_identity;

// Streaming stdout and validation-environment cases share this area.
// `signal_fanout` stays separate: it installs a process-wide SIGINT handler.
// `sandbox_profile_goldens` stays separate: it is the golden area selected
// by the golden gate.
#[cfg(unix)]
use orbit_common::OrbitError;
#[cfg(unix)]
use orbit_common::test_env::SupervisorSeam;
#[cfg(unix)]
use orbit_common::test_env::with_supervision_clock;
#[cfg(unix)]
use orbit_common::test_env::with_supervisor_seam;
#[cfg(unix)]
use orbit_exec::EnvironmentMode;
#[cfg(unix)]
use orbit_exec::ExecRequest;
#[cfg(unix)]
use orbit_exec::LoginShell;
#[cfg(unix)]
use orbit_exec::LoginShellMode;
#[cfg(unix)]
use orbit_exec::NoSandbox;
#[cfg(unix)]
use orbit_exec::Sandbox;
#[cfg(unix)]
use orbit_exec::StdinMode;
#[cfg(unix)]
use orbit_exec::ValidationEnvPolicy;
#[cfg(unix)]
use orbit_exec::ValidationEnvSource;
#[cfg(unix)]
use orbit_exec::ValidationEnvironment;
#[cfg(unix)]
use orbit_exec::ValidationPathMode;
#[cfg(unix)]
use orbit_exec::run_process;
#[cfg(unix)]
use orbit_exec::run_process_streaming_stdout;
#[cfg(unix)]
use std::cell::Cell;
#[cfg(unix)]
use std::cell::RefCell;
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::io::Read;
#[cfg(unix)]
use std::os::fd::FromRawFd;
#[cfg(unix)]
use std::os::fd::OwnedFd;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::process::Child;
#[cfg(unix)]
use std::process::Command;
#[cfg(unix)]
use std::process::Stdio;
#[cfg(unix)]
use std::rc::Rc;
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::sync::atomic::AtomicBool;
#[cfg(unix)]
use std::sync::atomic::Ordering;
#[cfg(unix)]
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;
#[cfg(unix)]
use tempfile::TempDir;
#[cfg(unix)]
use wait_timeout::ChildExt;

#[cfg(unix)]
include!("streaming_stdout.rs");
#[cfg(unix)]
include!("validation_env.rs");
