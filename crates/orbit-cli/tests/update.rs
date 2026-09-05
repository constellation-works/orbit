#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Binary-level coverage for `orbit update`.
//!
//! The deep replacement/rollback/convergence paths are unit-tested in
//! `orbit-cmd` against a fake installation. What can only be checked here is
//! the wiring: that the command is reachable, documents both version
//! selections, reports through the shared payload machinery, and refuses to
//! rewrite a binary a package manager owns — which is exactly the shape this
//! test binary itself has, since it lives in `target/`.

use std::path::Path;

use assert_cmd::cargo::cargo_bin_cmd;
use serde_json::Value;
use tempfile::tempdir;

fn orbit(cwd: &Path, home: &Path, mirror: &Path) -> assert_cmd::Command {
    let mut command = cargo_bin_cmd!("orbit");
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("ORBIT_UPDATE_RELEASE_DIR", mirror)
        .env_remove("ORBIT_HOME")
        .env_remove("ORBIT_ROOT")
        .env_remove("ORBIT_INSTALL_DIR")
        .env_remove("ORBIT_INSTALL_REPO")
        .env_remove("ORBIT_REGISTRY_ROOT")
        .env_remove("ORBIT_WORKSPACE");
    command
}

fn mirror_publishing(version: &str) -> tempfile::TempDir {
    let mirror = tempdir().expect("mirror tempdir");
    std::fs::write(mirror.path().join("latest-version.txt"), version).expect("write latest");
    mirror
}

#[test]
fn help_documents_both_latest_and_explicit_version_selection() {
    let home = tempdir().expect("home");
    let mirror = mirror_publishing("9.9.9");
    let output = orbit(home.path(), home.path(), mirror.path())
        .args(["update", "--help"])
        .output()
        .expect("run update --help");

    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("latest published Orbit release"), "{help}");
    assert!(help.contains("--version <VERSION>"), "{help}");
    assert!(help.contains("--check"), "{help}");
    assert!(help.contains("--allow-downgrade"), "{help}");
}

#[test]
fn check_reports_the_available_release_and_exits_three() {
    let home = tempdir().expect("home");
    let mirror = mirror_publishing("9.9.9");
    let output = orbit(home.path(), home.path(), mirror.path())
        .args(["update", "--check", "--json"])
        .output()
        .expect("run update --check");

    assert_eq!(output.status.code(), Some(3), "{output:?}");
    let report: Value =
        serde_json::from_slice(&output.stdout).expect("check report is machine-readable");
    assert_eq!(report["outcome"], "update_available");
    assert_eq!(report["target_version"], "9.9.9");
    assert_eq!(report["current_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(report["replaced"], false);
    // This test binary is a checkout build, so the report names the channel
    // and the command that actually upgrades it.
    assert_eq!(report["install_channel"], "local-build");
    assert_eq!(report["updatable"], false);
    assert!(
        report["remediation"]
            .as_str()
            .expect("remediation")
            .contains("make install"),
        "{report}"
    );
}

#[test]
fn check_is_quiet_and_succeeds_when_the_installed_release_is_current() {
    let home = tempdir().expect("home");
    let mirror = mirror_publishing(env!("CARGO_PKG_VERSION"));
    let output = orbit(home.path(), home.path(), mirror.path())
        .args(["update", "--check", "--json"])
        .output()
        .expect("run update --check");

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).expect("check report");
    assert_eq!(report["outcome"], "already_current");
}

#[test]
fn applying_an_update_to_a_checkout_build_is_refused_with_the_command_that_works() {
    let home = tempdir().expect("home");
    let mirror = mirror_publishing("9.9.9");
    let output = orbit(home.path(), home.path(), mirror.path())
        .arg("update")
        .output()
        .expect("run update");

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot update"), "{stderr}");
    assert!(stderr.contains("make install"), "{stderr}");
    assert!(
        output.stdout.is_empty(),
        "a failed command emits no payload"
    );
}

#[test]
fn an_unparseable_requested_version_is_rejected_before_any_network_or_disk_work() {
    let home = tempdir().expect("home");
    // No mirror content at all: the version is rejected before it is consulted.
    let mirror = tempdir().expect("mirror");
    let output = orbit(home.path(), home.path(), mirror.path())
        .args(["update", "--version", "latest"])
        .output()
        .expect("run update");

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("MAJOR.MINOR.PATCH"), "{stderr}");
}
