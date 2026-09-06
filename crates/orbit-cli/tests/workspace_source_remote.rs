#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Binary-level coverage for explicit workspace source-remote rebinding.

use std::fs;
use std::path::Path;
use std::process::Command as StdCommand;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::tempdir;

const OLD_REMOTE: &str = "git@github.com:example/orbit.git";
const NEW_REMOTE: &str = "ssh://github.com/example/orbit-renamed.git";

#[test]
fn owner_can_dry_run_apply_read_back_and_retry_source_remote_rebind() {
    let home = tempdir().expect("home");
    let repo = tempdir().expect("repo");
    init_git_repo(repo.path(), OLD_REMOTE);
    initialize_workspace(repo.path(), home.path(), "remote-owner");

    let task = run_json(
        repo.path(),
        home.path(),
        &[
            "task",
            "add",
            "--title",
            "Preserved across source move",
            "--description",
            "Disposable source-remote fixture",
            "--acceptance-criteria",
            "The task remains readable",
            "--complexity",
            "low",
            "--model",
            "codex",
            "--json",
        ],
    );
    let task_id = task["id"].as_str().expect("created task id").to_string();

    let registry_path = home.path().join(".orbit/workspaces.json");
    let initial_registry: Value =
        serde_json::from_slice(&fs::read(&registry_path).expect("initial registry"))
            .expect("parse initial registry");
    let initial_workspace = initial_registry["workspaces"][0].clone();
    let initial_checkouts = initial_registry["checkouts"].clone();

    let inspected = run_json(
        repo.path(),
        home.path(),
        &["workspace", "source-remote", "show", "--json"],
    );
    assert_eq!(inspected["remote"], OLD_REMOTE);
    assert_eq!(inspected["repository_identity"], "github.com/example/orbit");

    let before_dry_run = fs::read(&registry_path).expect("registry before dry run");
    let dry_run = run_json(
        repo.path(),
        home.path(),
        &[
            "workspace",
            "source-remote",
            "rebind",
            "--remote",
            NEW_REMOTE,
            "--dry-run",
            "--json",
        ],
    );
    assert_eq!(dry_run["action"], "would_rebind");
    assert_eq!(dry_run["changed"], true);
    assert_eq!(dry_run["dry_run"], true);
    assert_eq!(
        fs::read(&registry_path).expect("registry after dry run"),
        before_dry_run
    );

    let applied = run_json(
        repo.path(),
        home.path(),
        &[
            "workspace",
            "source-remote",
            "rebind",
            "--remote",
            NEW_REMOTE,
            "--json",
        ],
    );
    assert_eq!(applied["action"], "rebound");
    assert_eq!(
        applied["old"]["repository_identity"],
        "github.com/example/orbit"
    );
    assert_eq!(
        applied["new"]["repository_identity"],
        "github.com/example/orbit-renamed"
    );

    let rebound_registry: Value =
        serde_json::from_slice(&fs::read(&registry_path).expect("rebound registry"))
            .expect("parse rebound registry");
    assert_eq!(
        rebound_registry["workspaces"][0]["id"],
        initial_workspace["id"]
    );
    assert_eq!(
        rebound_registry["workspaces"][0]["owner_machine_id"],
        initial_workspace["owner_machine_id"]
    );
    assert_eq!(rebound_registry["checkouts"], initial_checkouts);
    assert_eq!(rebound_registry["workspaces"][0]["git_remote"], NEW_REMOTE);
    let preserved_task = run_json(
        repo.path(),
        home.path(),
        &["task", "show", &task_id, "--json"],
    );
    assert_eq!(preserved_task["id"], task_id);
    assert_eq!(preserved_task["title"], "Preserved across source move");

    let before_retry = fs::read(&registry_path).expect("registry before retry");
    let retry = run_json(
        repo.path(),
        home.path(),
        &[
            "workspace",
            "source-remote",
            "rebind",
            "--remote",
            "https://github.com/Example/Orbit-Renamed.git",
            "--json",
        ],
    );
    assert_eq!(retry["action"], "unchanged");
    assert_eq!(retry["changed"], false);
    assert_eq!(
        fs::read(&registry_path).expect("registry after retry"),
        before_retry
    );

    let verified = run_json(
        repo.path(),
        home.path(),
        &["workspace", "source-remote", "show", "--json"],
    );
    assert_eq!(verified["remote"], NEW_REMOTE);
}

#[test]
fn credential_bearing_rebind_is_redacted_and_does_not_mutate() {
    let home = tempdir().expect("home");
    let repo = tempdir().expect("repo");
    init_git_repo(repo.path(), OLD_REMOTE);
    initialize_workspace(repo.path(), home.path(), "remote-owner");
    let registry_path = home.path().join(".orbit/workspaces.json");
    let before = fs::read(&registry_path).expect("registry before rejection");

    orbit(repo.path(), home.path())
        .args([
            "workspace",
            "source-remote",
            "rebind",
            "--remote",
            "https://operator:supersecret@github.com/example/orbit.git",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("must not contain credentials"))
        .stderr(predicate::str::contains("supersecret").not());

    assert_eq!(
        fs::read(&registry_path).expect("registry after rejection"),
        before
    );
}

#[test]
fn source_remote_help_names_inspection_dry_run_and_rebind() {
    let home = tempdir().expect("home");
    let cwd = tempdir().expect("cwd");
    orbit(cwd.path(), home.path())
        .args(["workspace", "source-remote", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("show"))
        .stdout(predicate::str::contains("rebind"))
        .stdout(predicate::str::contains("verify"))
        .stdout(predicate::str::contains("roll back"))
        .stdout(predicate::str::contains("publication bindings"));
    orbit(cwd.path(), home.path())
        .args(["workspace", "source-remote", "rebind", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--dry-run"))
        .stdout(predicate::str::contains("Credentials"));
}

fn initialize_workspace(repo: &Path, home: &Path, host_name: &str) {
    orbit(repo, home)
        .args([
            "init",
            "--non-interactive",
            "--host-name",
            host_name,
            "--task-prefix",
            "TST",
        ])
        .assert()
        .success();
    orbit(repo, home)
        .args([
            "workspace",
            "init",
            "--name",
            "orbit",
            "--base-branch",
            "agent-main",
        ])
        .assert()
        .success();
}

fn run_json(cwd: &Path, home: &Path, args: &[&str]) -> Value {
    let output = orbit(cwd, home).args(args).assert().success();
    serde_json::from_slice(&output.get_output().stdout).expect("parse command JSON")
}

fn orbit(cwd: &Path, home: &Path) -> assert_cmd::Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("ORBIT_HOME");
    command
}

fn init_git_repo(repo: &Path, remote: &str) {
    run_git(repo, &["init"]);
    run_git(repo, &["remote", "add", "origin", remote]);
}

fn run_git(cwd: &Path, args: &[&str]) {
    let output = StdCommand::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git -C {} {} failed: {}",
        cwd.display(),
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}
