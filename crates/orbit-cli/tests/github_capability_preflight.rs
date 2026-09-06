#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! `github.auth.status` on an execution lane that holds no GitHub credentials.
//!
//! This is the lane GitHub-backed CI discovery has to survive: a sandbox that
//! denies reading the GitHub CLI's configuration and an execution environment
//! that forwards no GitHub token. The preflight must come back as an ordinary
//! result naming the missing capability — not as a tool error, and never as
//! something a caller could mistake for a clean CI pipeline.

use std::path::{Path, PathBuf};

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::{TempDir, tempdir};

struct Lane {
    _temp: TempDir,
    home: PathBuf,
    work: PathBuf,
    path: PathBuf,
}

impl Lane {
    /// A workspace whose `PATH` holds only what the fixture puts there, so a
    /// GitHub CLI is present exactly when a test installs one.
    fn new() -> Self {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        let path = temp.path().join("bin");
        for dir in [&home, &work, &path] {
            std::fs::create_dir_all(dir).expect("create fixture dir");
        }
        Self {
            _temp: temp,
            home,
            work,
            path,
        }
    }

    /// Install a stand-in `gh` that behaves like an unauthenticated one: it
    /// runs, and it reports that it has no usable credentials.
    fn install_unauthenticated_gh(&self) {
        let script = "#!/bin/sh\necho 'gh: To get started with GitHub CLI, please run: gh auth login' >&2\nexit 1\n";
        let gh = self.path.join("gh");
        std::fs::write(&gh, script).expect("write gh stub");
        set_executable(&gh);
    }

    fn preflight(&self) -> Value {
        let mut command = cargo_bin_cmd!("orbit");
        // ORB-11300: `tool run` resolves its runtime from ambient routing
        // before it ever reaches the stub `gh` on PATH.
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        let output = command
            .current_dir(&self.work)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("PATH", &self.path)
            .env_remove("GH_TOKEN")
            .env_remove("GITHUB_TOKEN")
            .args(["tool", "run", "github.auth.status"])
            .output()
            .expect("run github.auth.status");

        assert!(
            output.status.success(),
            "a lane without credentials must still complete the preflight: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("preflight returns JSON")
    }
}

#[cfg(unix)]
fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)
        .expect("stub metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("mark stub executable");
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) {}

#[test]
fn a_lane_without_a_github_cli_reports_the_surface_unavailable() {
    let lane = Lane::new();

    let preflight = lane.preflight();

    assert_eq!(preflight["available"], false);
    assert_eq!(preflight["authenticated"], false);
    assert!(
        preflight["detail"].as_str().is_some_and(|d| !d.is_empty()),
        "the outcome must carry a reason a summary can quote: {preflight}"
    );
}

#[cfg(unix)]
#[test]
fn a_lane_with_no_credentials_reports_unauthenticated_rather_than_failing() {
    let lane = Lane::new();
    lane.install_unauthenticated_gh();

    let preflight = lane.preflight();

    assert_eq!(
        preflight["available"], true,
        "the client is present; only the credential is missing: {preflight}"
    );
    assert_eq!(preflight["authenticated"], false);
    assert!(
        preflight["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("no usable credentials")),
        "the outcome must name the missing credential: {preflight}"
    );
}
