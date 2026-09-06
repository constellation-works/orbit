#![allow(missing_docs)]
// Integration fixtures use expect for concise failure diagnostics.
#![allow(clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::process::{Command as StdCommand, Output};

use assert_cmd::Command as AssertCommand;
use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::{Value, json};
use tempfile::tempdir;

#[test]
fn managed_cli_proc_spawn_applies_registered_workspace_policy() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(workspace.join("allowed")).expect("create allowed directory");
    fs::create_dir_all(workspace.join("denied")).expect("create denied directory");
    fs::write(workspace.join("allowed/visible.txt"), "visible").expect("write allowed fixture");
    fs::write(workspace.join("denied/private.txt"), "private").expect("write denied fixture");
    init_git_repo(&workspace);
    workspace_init(&workspace, &home);
    fs::write(
        home.join(".orbit/resources/policies/default.yaml"),
        r#"schemaVersion: 2
kind: Policy
metadata:
  name: default
spec:
  description: Managed proc.spawn integration policy
  denyRead:
    - ./denied/**
  denyModify: []
  fsProfiles:
    restricted:
      read:
        - ./allowed/**
      modify: []
"#,
    )
    .expect("write restricted policy");

    let allowed = run_managed_proc_spawn(
        &workspace,
        &home,
        json!({
            "program": "/bin/cat",
            "args": ["allowed/visible.txt"],
            "timeout_ms": 5_000
        }),
        Some("restricted"),
    );
    assert!(
        allowed.status.success(),
        "allowed managed proc.spawn failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&allowed.stdout),
        String::from_utf8_lossy(&allowed.stderr)
    );
    let value: Value = serde_json::from_slice(&allowed.stdout).expect("allowed JSON output");
    assert_eq!(value["stdout"].as_str(), Some("visible"));

    let denied = run_managed_proc_spawn(
        &workspace,
        &home,
        json!({
            "program": "/bin/cat",
            "args": ["denied/private.txt"],
            "timeout_ms": 5_000
        }),
        Some("restricted"),
    );
    assert!(
        !denied.status.success(),
        "denied path unexpectedly executed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&denied.stdout),
        String::from_utf8_lossy(&denied.stderr)
    );
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&denied.stdout),
        String::from_utf8_lossy(&denied.stderr)
    );
    assert!(
        diagnostic.contains("proc.spawn path")
            && diagnostic.contains("fsProfile 'restricted'")
            && !diagnostic.contains("missing its resolved filesystem policy"),
        "expected the resolved profile to deny the path: {diagnostic}"
    );

    let missing_profile = run_managed_proc_spawn(
        &workspace,
        &home,
        json!({
            "program": "/bin/cat",
            "args": ["allowed/visible.txt"],
            "timeout_ms": 5_000
        }),
        None,
    );
    assert!(!missing_profile.status.success());
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&missing_profile.stdout),
        String::from_utf8_lossy(&missing_profile.stderr)
    );
    assert!(
        diagnostic.contains("missing its resolved filesystem policy"),
        "missing managed profile did not fail closed: {diagnostic}"
    );
}

fn workspace_init(workspace: &Path, home: &Path) {
    let output = orbit_command(workspace, home)
        .args(["workspace", "init"])
        .output()
        .expect("run workspace init");
    assert!(
        output.status.success(),
        "workspace init failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_managed_proc_spawn(
    workspace: &Path,
    home: &Path,
    input: Value,
    fs_profile: Option<&str>,
) -> Output {
    let mut command = orbit_command(workspace, home);
    command
        .env("ORBIT_MANAGED_RUN_CONTEXT", "1")
        .env("ORBIT_RUN_ID", "jrun-proc-spawn-test")
        .env("ORBIT_TASK_ACTOR_KIND", "agent")
        .env("ORBIT_ACTIVITY_TOOLS", "proc.spawn")
        .env("ORBIT_PROC_ALLOWED_PROGRAMS", "/bin/cat");
    match fs_profile {
        Some(profile) => {
            command.env("ORBIT_ACTIVITY_FS_PROFILE", profile);
        }
        None => {
            command.env_remove("ORBIT_ACTIVITY_FS_PROFILE");
        }
    }
    command
        .args(["tool", "run", "proc.spawn", "--input", &input.to_string()])
        .output()
        .expect("run managed proc.spawn")
}

fn orbit_command(workspace: &Path, home: &Path) -> AssertCommand {
    let mut command = cargo_bin_cmd!("orbit");
    // ORB-11300: `run_managed_proc_spawn` synthesizes its *own* managed run
    // context on top of this. Without clearing the inherited
    // `ORBIT_REGISTRY_ROOT`/`ORBIT_WORKSPACE` pair first, that synthetic
    // context would have pointed the sandboxed tool at the live workspace.
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(workspace)
        .env("HOME", home)
        .env("USERPROFILE", home);
    command
}

fn init_git_repo(workspace: &Path) {
    run_git(workspace, &["init"]);
    run_git(workspace, &["config", "user.name", "Orbit Test"]);
    run_git(
        workspace,
        &["config", "user.email", "orbit-test@example.com"],
    );
    run_git(workspace, &["config", "commit.gpgsign", "false"]);
    fs::write(workspace.join("README.md"), "# fixture\n").expect("write readme");
    run_git(workspace, &["add", "README.md"]);
    run_git(workspace, &["commit", "-m", "initial"]);
}

fn run_git(workspace: &Path, args: &[&str]) {
    let output = StdCommand::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git -C {} {} failed\nstdout:\n{}\nstderr:\n{}",
        workspace.display(),
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
