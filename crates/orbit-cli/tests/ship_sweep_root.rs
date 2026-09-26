#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::Path;
use std::process::Command as StdCommand;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::tempdir;

#[test]
fn ship_sweep_selects_explicit_environment_and_default_registries() {
    let fixture = tempdir().expect("fixture tempdir");
    let home = fixture.path().join("home");
    let custom_root = fixture.path().join("custom-root");
    let custom_repo = fixture.path().join("custom-repo");
    let home_repo = fixture.path().join("home-repo");
    let scheduler_cwd = fixture.path().join("scheduler");
    for dir in [&home, &custom_repo, &home_repo, &scheduler_cwd] {
        fs::create_dir_all(dir).expect("fixture directory");
    }
    init_git_repo(&custom_repo);
    init_git_repo(&home_repo);

    let root = custom_root.to_str().expect("UTF-8 custom root");
    orbit(
        &custom_repo,
        &home,
        None,
        &[
            "--root",
            root,
            "init",
            "--non-interactive",
            "--machine-name",
            "custom-host",
            "--task-prefix",
            "CR",
        ],
    );
    orbit(
        &custom_repo,
        &home,
        None,
        &["--root", root, "workspace", "init", "--name", "custom-only"],
    );
    assert!(
        !home.join(".orbit").exists(),
        "custom setup must not create the home registry"
    );

    let explicit = orbit_json(
        &scheduler_cwd,
        &home,
        None,
        &["--root", root, "run", "ship-sweep", "--dry-run", "--json"],
    );
    assert_workspace(&explicit, "custom-only");
    let from_env = orbit_json(
        &scheduler_cwd,
        &home,
        Some(&custom_root),
        &["run", "ship-sweep", "--dry-run", "--json"],
    );
    assert_workspace(&from_env, "custom-only");
    assert!(
        !home.join(".orbit").exists(),
        "an override must not fall back to HOME"
    );
    assert!(
        !scheduler_cwd.join(".orbit").exists(),
        "the scheduler cwd must remain untouched"
    );

    orbit(
        &home_repo,
        &home,
        None,
        &[
            "init",
            "--non-interactive",
            "--machine-name",
            "home-host",
            "--task-prefix",
            "HR",
        ],
    );
    orbit(
        &home_repo,
        &home,
        None,
        &["workspace", "init", "--name", "home-only"],
    );
    let default = orbit_json(
        &scheduler_cwd,
        &home,
        None,
        &["run", "ship-sweep", "--dry-run", "--json"],
    );
    assert_workspace(&default, "home-only");

    let explicit_wins = orbit_json(
        &scheduler_cwd,
        &home,
        Some(&home.join(".orbit")),
        &["--root", root, "run", "ship-sweep", "--dry-run", "--json"],
    );
    assert_workspace(&explicit_wins, "custom-only");
    assert!(
        !scheduler_cwd.join(".orbit").exists(),
        "sweep must not bootstrap the scheduler cwd"
    );
}

fn assert_workspace(output: &Value, expected_name: &str) {
    assert_eq!(output["dry_run"], true);
    assert_eq!(output["workspaces"], 1, "{output}");
    assert_eq!(
        output["reports"][0]["workspace_name"], expected_name,
        "{output}"
    );
    assert_eq!(output["reports"][0]["action"], "skipped", "{output}");
    assert_eq!(
        output["reports"][0]["skip_reason"], "auto_ship_disabled",
        "{output}"
    );
}

fn orbit_json(cwd: &Path, home: &Path, root_env: Option<&Path>, args: &[&str]) -> Value {
    let output = orbit(cwd, home, root_env, args);
    serde_json::from_slice(&output).expect("ship-sweep JSON output")
}

fn orbit(cwd: &Path, home: &Path, root_env: Option<&Path>, args: &[&str]) -> Vec<u8> {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home);
    if let Some(root) = root_env {
        command.env("ORBIT_ROOT", root);
    }
    command
        .args(args)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone()
}

fn init_git_repo(repo: &Path) {
    for args in [
        vec!["init", "--quiet"],
        vec!["config", "user.name", "Orbit Test"],
        vec!["config", "user.email", "orbit-test@example.com"],
        vec!["config", "commit.gpgsign", "false"],
    ] {
        let status = StdCommand::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git setup failed");
    }
    fs::write(repo.join("README.md"), "# fixture\n").expect("write fixture file");
    let status = StdCommand::new("git")
        .arg("-C")
        .arg(repo)
        .args(["add", "README.md"])
        .status()
        .expect("git add");
    assert!(status.success(), "git add failed");
    let status = StdCommand::new("git")
        .arg("-C")
        .arg(repo)
        .args(["commit", "--quiet", "-m", "initial"])
        .status()
        .expect("git commit");
    assert!(status.success(), "git commit failed");
}
