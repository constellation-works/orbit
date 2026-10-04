#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! `orbit workspace init` seeds `task_pilot.yaml` as a `preparation_eligible`
//! state routine bound to this host and the registered base branch
//! [ORB-12745]: the definition loads, `orbit routine show` reports the
//! resolved owner and branch, and a dry-run clock tick evaluates it as a
//! state trigger instead of failing on it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::tempdir;

#[test]
fn workspace_init_seeds_a_loadable_state_task_pilot_bound_to_this_host() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let repo = temp.path().join("repo");
    let root = temp.path().join("root");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&repo).expect("create repo");
    init_git_repo(&repo);
    let root_arg = root.to_string_lossy().into_owned();

    run_success(
        &repo,
        &home,
        &[
            "--root",
            &root_arg,
            "init",
            "--non-interactive",
            "--machine-name",
            "state-seed-host",
            "--task-prefix",
            "SS",
        ],
    );
    run_success(
        &repo,
        &home,
        &[
            "--root",
            &root_arg,
            "workspace",
            "init",
            "--name",
            "state-seed",
            "--base-branch",
            "trunk",
        ],
    );

    // With an explicit `--root`, the root is the workspace's shared `.orbit`.
    let path = root.join("routines/task_pilot.yaml");
    let seeded = fs::read_to_string(&path).expect("read seeded task-pilot routine");
    assert!(
        !seeded.contains("__ORBIT_"),
        "every placeholder resolves at seed time:\n{seeded}"
    );

    let list = run_json(
        &repo,
        &home,
        &["--root", &root_arg, "routine", "list", "--format", "json"],
    );
    let machine_id = list["machine_id"]
        .as_str()
        .unwrap_or_else(|| panic!("routine list reports this host's machine id: {list}"))
        .to_string();
    assert!(
        list["load_errors"]
            .as_array()
            .is_none_or(|errors| errors.is_empty()),
        "the seeded state routine must load without errors: {list}"
    );

    let shown = run_json(
        &repo,
        &home,
        &[
            "--root",
            &root_arg,
            "routine",
            "show",
            "task-pilot-state-seed",
            "--format",
            "json",
        ],
    );
    assert_eq!(shown["enabled"], false, "{shown}");
    assert_eq!(shown["target"], "job:task_pilot_pipeline", "{shown}");
    let trigger = &shown["trigger"]["state"];
    assert_eq!(trigger["kind"], "preparation_eligible", "{shown}");
    assert_eq!(trigger["owner_machine"], machine_id, "{shown}");
    assert_eq!(trigger["branch"], "trunk", "{shown}");
    assert_eq!(
        trigger["eligibility"]["statuses"],
        serde_json::json!(["proposed", "backlog"])
    );
    assert_eq!(
        trigger["eligibility"]["exclude_tags"],
        serde_json::json!(["no-diff-expected", "no-diff-needed"])
    );

    // Disabled as seeded: the tick evaluates the state trigger and reports
    // the definition's own switch, never a load or evaluation error.
    let tick = run_json(
        &repo,
        &home,
        &[
            "--root",
            &root_arg,
            "clock",
            "tick",
            "--dry-run",
            "--format",
            "json",
        ],
    );
    assert_eq!(state_report(&tick)["reason"], "disabled", "{tick}");

    // Opted in, the same definition evaluates its (empty) backlog as fresh.
    fs::write(&path, seeded.replace("enabled: false", "enabled: true")).expect("opt in");
    let tick = run_json(
        &repo,
        &home,
        &[
            "--root",
            &root_arg,
            "clock",
            "tick",
            "--dry-run",
            "--format",
            "json",
        ],
    );
    let report = state_report(&tick);
    assert!(
        matches!(report["reason"].as_str(), Some("fresh" | "debouncing")),
        "an enabled seeded state routine evaluates cleanly: {tick}"
    );
}

fn state_report(tick: &Value) -> &Value {
    assert!(
        tick["load_errors"]
            .as_array()
            .is_none_or(|errors| errors.is_empty()),
        "{tick}"
    );
    tick["reports"]
        .as_array()
        .and_then(|reports| {
            reports
                .iter()
                .find(|report| report["routine"] == "task-pilot-state-seed")
        })
        .unwrap_or_else(|| panic!("task-pilot state report in tick output: {tick}"))
}

fn command(cwd: &Path, home: &Path, args: &[&str]) -> assert_cmd::Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home);
    if let Some(root) = args
        .windows(2)
        .find_map(|pair| (pair[0] == "--root").then(|| PathBuf::from(pair[1])))
    {
        command
            .env("ORBIT_MANAGED_RUN_CONTEXT", "1")
            .env("ORBIT_RUN_ID", "routine-state-seed-test")
            .env("ORBIT_REGISTRY_ROOT", root);
    }
    command.args(args);
    command
}

fn run_success(cwd: &Path, home: &Path, args: &[&str]) {
    command(cwd, home, args).assert().success();
}

fn run_json(cwd: &Path, home: &Path, args: &[&str]) -> Value {
    let output = command(cwd, home, args)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&output).unwrap_or_else(|error| {
        panic!(
            "parse JSON from `orbit {}`: {error}\nstdout:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output)
        )
    })
}

fn init_git_repo(repo: &Path) {
    run_git(repo, &["init", "--quiet", "--initial-branch=trunk"]);
    run_git(repo, &["config", "user.name", "Orbit Test"]);
    run_git(repo, &["config", "user.email", "orbit-test@example.com"]);
    run_git(repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("README.md"), "# state seed test\n").expect("write readme");
    run_git(repo, &["add", "README.md"]);
    run_git(repo, &["commit", "--quiet", "-m", "initial"]);
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
        "git -C {} {} failed\nstdout:\n{}\nstderr:\n{}",
        cwd.display(),
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
