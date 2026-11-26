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
mod script_sandbox;

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

// `validation_env.rs` fixtures; the included files share this scope, so
// their constants and types precede every included function.

/// What `env -i PATH=/usr/bin:/bin orbit run auto …` hands the worker.
#[cfg(unix)]
const MINIMAL_PATH: &str = "/usr/bin:/bin";

#[cfg(unix)]
struct Host {
    _dir: TempDir,
    home: PathBuf,
    tools: PathBuf,
}

#[cfg(unix)]
impl Host {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let tools = home.join(".toolchain/bin");
        fs::create_dir_all(&tools).unwrap();
        executable(
            &tools.join("orbit-fake-tool-13987"),
            "#!/bin/sh\necho toolchain-ok\n",
        );
        Self {
            _dir: dir,
            home,
            tools,
        }
    }

    /// The minimal environment a non-interactive launcher provides.
    fn launcher_env(&self) -> Vec<(String, String)> {
        vec![
            ("HOME".to_string(), self.home.display().to_string()),
            ("PATH".to_string(), MINIMAL_PATH.to_string()),
            ("USER".to_string(), "orbit-test".to_string()),
        ]
    }

    /// A login shell whose profile prints a banner, extends PATH, exports a
    /// toolchain locator and an unrelated secret, then runs `-c`.
    fn fake_login_shell(&self, name: &str, body_before_exec: &str) -> PathBuf {
        let shell = self.home.join(name);
        executable(
            &shell,
            &format!(
                "#!/bin/sh\n\
                 [ \"$1\" = -i ] && shift\n\
                 [ \"$1\" = -l ] && [ \"$2\" = -c ] || exit 64\n\
                 echo 'Welcome to the fake login shell'\n\
                 {body_before_exec}\n\
                 PATH=\"{tools}:$PATH\"; export PATH\n\
                 CARGO_HOME=\"$HOME/.cargo\"; export CARGO_HOME\n\
                 ORBIT_TEST_LOGIN_SECRET=leaked; export ORBIT_TEST_LOGIN_SECRET\n\
                 eval \"$3\"\n",
                tools = self.tools.display()
            ),
        );
        shell
    }

    /// A real bash login profile sources an rc with the usual interactive
    /// guard. Profile logging records startup reaching the profile, which a
    /// timed-out probe need not do, without using real dotfiles.
    fn bash_rc(&self, profile_setup: &str, interactive_setup: &str) {
        fs::write(
            self.home.join(".bash_profile"),
            format!(
                "export PATH={MINIMAL_PATH}\n\
                 printf '%s\\n' \"$-\" >> \"$HOME/probes\"\n\
                 {profile_setup}\n\
                 . \"$HOME/.bashrc\"\n"
            ),
        )
        .unwrap();
        fs::write(
            self.home.join(".bashrc"),
            format!(
                "case $- in *i*) ;; *) return ;; esac\n\
                 echo 'rc banner'\n\
                 {interactive_setup}\n"
            ),
        )
        .unwrap();
    }

    fn login_path(&self) -> String {
        format!("{}:{MINIMAL_PATH}", self.tools.display())
    }
}

#[cfg(unix)]
include!("streaming_stdout.rs");
#[cfg(unix)]
include!("validation_env.rs");
