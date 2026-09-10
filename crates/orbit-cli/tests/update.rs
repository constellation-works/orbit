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

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::tempdir;

fn orbit(cwd: &Path, home: &Path, mirror: &Path) -> assert_cmd::Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
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

fn run_git(repo: &Path, args: &[&str]) {
    let output = StdCommand::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {} failed: {output:?}",
        args.join(" ")
    );
}

fn init_git_repo(repo: &Path) {
    fs::create_dir_all(repo).expect("create repository");
    run_git(repo, &["init", "--quiet"]);
    run_git(repo, &["config", "user.name", "Orbit Test"]);
    run_git(repo, &["config", "user.email", "orbit-test@example.com"]);
    run_git(repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("README.md"), "# update root test\n").expect("write readme");
    run_git(repo, &["add", "README.md"]);
    run_git(repo, &["commit", "--quiet", "-m", "initial"]);
}

fn initialize_root(repo: &Path, home: &Path, mirror: &Path, root: &Path, suffix: &str) {
    let root = root.to_string_lossy();
    let host_name = format!("update-{suffix}");
    orbit(repo, home, mirror)
        .args([
            "--root",
            root.as_ref(),
            "init",
            "--non-interactive",
            "--host-name",
            &host_name,
            "--task-prefix",
            "UP",
        ])
        .assert()
        .success();
    let workspace_name = format!("update-{suffix}");
    orbit(repo, home, mirror)
        .args([
            "--root",
            root.as_ref(),
            "workspace",
            "init",
            "--name",
            &workspace_name,
        ])
        .assert()
        .success();
}

fn installed_orbit(
    executable: &Path,
    cwd: &Path,
    home: &Path,
    mirror: &Path,
) -> assert_cmd::Command {
    let mut command = assert_cmd::Command::new(executable);
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("ORBIT_UPDATE_RELEASE_DIR", mirror)
        .env(
            "ORBIT_INSTALL_DIR",
            executable.parent().expect("managed install directory"),
        )
        .env_remove("ORBIT_HOME")
        .env_remove("ORBIT_INSTALL_REPO");
    command
}

fn install_test_binary(directory: &Path) -> PathBuf {
    fs::create_dir_all(directory).expect("create managed install directory");
    let executable = directory.join("orbit");
    fs::copy(env!("CARGO_BIN_EXE_orbit"), &executable).expect("copy tested Orbit binary");
    executable
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn update_preserves_explicit_and_environment_roots_from_another_checkout() {
    let temp = tempdir().expect("fixture tempdir");
    let home = temp.path().join("home");
    let checkout_b = temp.path().join("checkout-b");
    let root_a = temp.path().join("root-a");
    let root_b = temp.path().join("root-b");
    fs::create_dir_all(&home).expect("create home");
    init_git_repo(&checkout_b);
    let mirror = mirror_publishing(env!("CARGO_PKG_VERSION"));
    initialize_root(&checkout_b, &home, mirror.path(), &root_a, "a");
    initialize_root(&checkout_b, &home, mirror.path(), &root_b, "b");

    let marker_a = root_a.join("state/layout.version");
    let marker_b = root_b.join("state/layout.version");
    fs::write(&marker_a, "1\n").expect("make root A migration pending");
    fs::write(&marker_b, "99\n").expect("make root B incompatible");
    let managed_a = root_a.join("auto_tasks/code-review.yaml");
    let managed_b = root_b.join("auto_tasks/code-review.yaml");
    fs::remove_file(&managed_a).expect("remove root A managed asset");
    fs::remove_file(&managed_b).expect("remove root B managed asset");

    let executable = install_test_binary(&temp.path().join("managed-bin"));
    let root_a_arg = root_a.to_string_lossy();
    let output = installed_orbit(&executable, &checkout_b, &home, mirror.path())
        .env("ORBIT_ROOT", &root_b)
        .args(["update", "--root", root_a_arg.as_ref(), "--json"])
        .output()
        .expect("run update with explicit root A");
    assert!(
        output.status.success(),
        "explicit-root update failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("parse update report");
    assert_eq!(report["workspace_root"], root_a.to_string_lossy().as_ref());
    assert_eq!(report["steps"][0]["command"], "migrate --confirm");
    assert_eq!(report["steps"][0]["status"], "succeeded");
    assert_eq!(report["steps"][1]["command"], "workspace sync");
    assert_eq!(report["steps"][1]["status"], "succeeded");
    assert_ne!(
        fs::read_to_string(&marker_a).expect("read migrated root A marker"),
        "1\n"
    );
    assert_eq!(
        fs::read_to_string(&marker_b).expect("read untouched root B marker"),
        "99\n"
    );
    assert!(managed_a.exists(), "sync did not restore root A asset");
    assert!(!managed_b.exists(), "sync unexpectedly touched root B");

    let compatibility = installed_orbit(&executable, &checkout_b, &home, mirror.path())
        .env("ORBIT_ROOT", &root_b)
        .args([
            "migrate",
            "--dry-run",
            "--root",
            root_a_arg.as_ref(),
            "--json",
        ])
        .output()
        .expect("run downgrade compatibility inspection against root A");
    assert!(
        compatibility.status.success(),
        "root A compatibility inspection failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&compatibility.stdout),
        String::from_utf8_lossy(&compatibility.stderr)
    );
    let compatibility: Value =
        serde_json::from_slice(&compatibility.stdout).expect("parse compatibility report");
    assert_eq!(
        compatibility["orbit_dir"],
        root_a.to_string_lossy().as_ref()
    );
    assert_eq!(
        fs::read_to_string(&marker_b).expect("reread untouched root B marker"),
        "99\n"
    );

    fs::remove_file(&managed_a).expect("remove root A asset for environment-only retry");
    let retry = installed_orbit(&executable, &checkout_b, &home, mirror.path())
        .env("ORBIT_ROOT", &root_a)
        .args(["update", "--json"])
        .output()
        .expect("run update with environment root A");
    assert!(
        retry.status.success(),
        "environment-root update failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&retry.stdout),
        String::from_utf8_lossy(&retry.stderr)
    );
    assert!(managed_a.exists(), "environment-only root did not reach A");
    assert!(!managed_b.exists(), "environment-only update touched B");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn update_without_a_root_override_retains_default_workspace_routing() {
    let temp = tempdir().expect("fixture tempdir");
    let home = temp.path().join("home");
    let repo = temp.path().join("repo");
    fs::create_dir_all(&home).expect("create home");
    init_git_repo(&repo);
    let mirror = mirror_publishing(env!("CARGO_PKG_VERSION"));
    orbit(&repo, &home, mirror.path())
        .args([
            "init",
            "--non-interactive",
            "--host-name",
            "update-default",
            "--task-prefix",
            "UD",
        ])
        .assert()
        .success();
    orbit(&repo, &home, mirror.path())
        .args(["workspace", "init", "--name", "update-default"])
        .assert()
        .success();

    let workspace_root = repo.join(".orbit");
    let managed = workspace_root.join("auto_tasks/code-review.yaml");
    fs::remove_file(&managed).expect("remove default-root managed asset");
    let executable = install_test_binary(&temp.path().join("managed-bin"));
    let output = installed_orbit(&executable, &repo, &home, mirror.path())
        .args(["update", "--json"])
        .output()
        .expect("run default-root update");
    assert!(
        output.status.success(),
        "default-root update failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("parse update report");
    assert_eq!(
        report["workspace_root"],
        workspace_root
            .canonicalize()
            .expect("canonical workspace root")
            .to_string_lossy()
            .as_ref()
    );
    assert!(
        managed.exists(),
        "default routing did not sync the workspace"
    );
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
    assert!(
        !report["remediation"]
            .as_str()
            .expect("remediation")
            .contains("invalid input: "),
        "{report}"
    );
}

#[test]
fn check_renders_a_prefix_free_remediation_sentence() {
    let home = tempdir().expect("home");
    let mirror = mirror_publishing("9.9.9");
    let output = orbit(home.path(), home.path(), mirror.path())
        .args(["update", "--check"])
        .output()
        .expect("run update --check");

    assert_eq!(output.status.code(), Some(3), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("An update is available, but cannot update '")
            && text.contains(
                "in place: this is a local build inside a checkout; update the checkout and rebuild (`git pull && make install`)."
            ),
        "{text}"
    );
    assert!(!text.contains("invalid input: "), "{text}");
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
