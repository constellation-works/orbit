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
fn global_workspace_flag_selects_by_name_and_id_from_a_foreign_checkout() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let orbit_repo = temp.path().join("orbit");
    let other_repo = temp.path().join("other");
    let elsewhere = temp.path().join("elsewhere");
    fs::create_dir_all(&home).expect("home");
    fs::create_dir_all(&elsewhere).expect("elsewhere");

    init_git_repo(&orbit_repo);
    init_git_repo(&other_repo);

    run_orbit(
        &orbit_repo,
        &home,
        &[
            "init",
            "--non-interactive",
            "--host-name",
            "selector-host",
            "--task-prefix",
            "SEL",
        ],
    )
    .success();
    run_orbit(
        &orbit_repo,
        &home,
        &["workspace", "init", "--name", "orbit"],
    )
    .success();
    run_orbit(
        &other_repo,
        &home,
        &["workspace", "init", "--name", "other"],
    )
    .success();

    let created = run_orbit_json(
        &orbit_repo,
        &home,
        &[
            "task",
            "add",
            "--title",
            "Orbit-only task",
            "--description",
            "Must stay in the orbit workspace",
            "--complexity",
            "low",
            "--json",
        ],
    );
    let orbit_task_id = created["id"].as_str().expect("created id").to_string();

    let by_name = run_orbit_json(
        &elsewhere,
        &home,
        &[
            "--workspace",
            "orbit",
            "task",
            "list",
            "--limit",
            "10",
            "--json",
        ],
    );
    assert!(
        task_ids(&by_name).contains(&orbit_task_id),
        "name selector from a foreign cwd must list orbit tasks: {by_name}"
    );

    let by_id = run_orbit_json(
        &elsewhere,
        &home,
        &[
            "--workspace",
            "ws_orbit",
            "task",
            "list",
            "--limit",
            "10",
            "--json",
        ],
    );
    assert!(
        task_ids(&by_id).contains(&orbit_task_id),
        "logical id selector from a foreign cwd must list orbit tasks: {by_id}"
    );

    let by_path = run_orbit_json(
        &elsewhere,
        &home,
        &[
            "--workspace",
            orbit_repo.to_str().expect("utf8"),
            "task",
            "list",
            "--limit",
            "10",
            "--json",
        ],
    );
    assert!(
        task_ids(&by_path).contains(&orbit_task_id),
        "absolute checkout path must list orbit tasks: {by_path}"
    );

    let other_cwd = run_orbit_json(
        &other_repo,
        &home,
        &["task", "list", "--limit", "10", "--json"],
    );
    assert!(
        !task_ids(&other_cwd).contains(&orbit_task_id),
        "cwd discovery without --workspace must keep binding the other checkout: {other_cwd}"
    );
}

#[test]
fn global_workspace_flag_fails_closed_on_unknown_selector() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let repo = temp.path().join("repo");
    fs::create_dir_all(&home).expect("home");
    init_git_repo(&repo);
    run_orbit(
        &repo,
        &home,
        &[
            "init",
            "--non-interactive",
            "--host-name",
            "selector-host",
            "--task-prefix",
            "SEL",
        ],
    )
    .success();
    run_orbit(&repo, &home, &["workspace", "init", "--name", "orbit"]).success();

    let assert = run_orbit(
        &repo,
        &home,
        &[
            "--workspace",
            "no-such-workspace",
            "task",
            "list",
            "--limit",
            "1",
        ],
    )
    .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(
        stderr.contains("no-such-workspace"),
        "unknown selector must be named: {stderr}"
    );
}

/// `task show` is the one verb whose target is a machine-global primary key, so
/// omitting `--workspace` follows the ID instead of the cwd [ORB-10797].
#[test]
fn task_show_follows_the_global_task_id_and_explicit_workspace_stays_a_filter() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let orbit_repo = temp.path().join("orbit");
    let other_repo = temp.path().join("other");
    let elsewhere = temp.path().join("elsewhere");
    fs::create_dir_all(&home).expect("home");
    fs::create_dir_all(&elsewhere).expect("elsewhere");

    init_git_repo(&orbit_repo);
    init_git_repo(&other_repo);

    run_orbit(
        &orbit_repo,
        &home,
        &[
            "init",
            "--non-interactive",
            "--host-name",
            "selector-host",
            "--task-prefix",
            "SEL",
        ],
    )
    .success();
    run_orbit(
        &orbit_repo,
        &home,
        &["workspace", "init", "--name", "orbit"],
    )
    .success();
    run_orbit(
        &other_repo,
        &home,
        &["workspace", "init", "--name", "other"],
    )
    .success();

    let created = run_orbit_json(
        &orbit_repo,
        &home,
        &[
            "task",
            "add",
            "--title",
            "Globally addressable task",
            "--description",
            "Reachable by ID from anywhere",
            "--complexity",
            "low",
            "--json",
        ],
    );
    let task_id = created["id"].as_str().expect("created id").to_string();

    // A foreign checkout, and a directory that is no workspace at all.
    for cwd in [&other_repo, &elsewhere] {
        let shown = run_orbit_json(cwd, &home, &["task", "show", &task_id, "--json"]);
        assert_eq!(
            shown["id"],
            Value::String(task_id.clone()),
            "task show from {} must follow the id: {shown}",
            cwd.display()
        );
        assert_eq!(shown["workspace"]["name"], "orbit");
        assert_eq!(shown["workspace"]["id"], "ws_orbit");
    }

    let human = run_orbit(&elsewhere, &home, &["task", "show", &task_id]).success();
    let stdout = String::from_utf8_lossy(&human.get_output().stdout).into_owned();
    assert!(
        stdout.contains("Workspace: orbit (ws_orbit)"),
        "human output must name the owning workspace: {stdout}"
    );

    // An explicit selector is a filter: the task is not in `other`, so the read
    // fails closed rather than falling back to the owner.
    let missed = run_orbit(
        &elsewhere,
        &home,
        &["--workspace", "other", "task", "show", &task_id],
    )
    .failure();
    let missed_stdout = String::from_utf8_lossy(&missed.get_output().stdout);
    assert!(
        !missed_stdout.contains("Globally addressable task"),
        "a foreign task must not be printed under `--workspace other`: {missed_stdout}"
    );
    let missed_stderr = String::from_utf8_lossy(&missed.get_output().stderr);
    assert!(
        missed_stderr.contains(&task_id),
        "the miss must name the task it looked for: {missed_stderr}"
    );
}

/// `orbit tool run orbit.task.show` is the agent-facing twin of `orbit task show`
/// and must follow the ID from a foreign checkout and from no workspace at all
/// [ORB-10961]. An explicit `workspace` in the tool input stays a filter.
#[test]
fn tool_run_task_show_follows_the_global_task_id_and_explicit_workspace_stays_a_filter() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let orbit_repo = temp.path().join("orbit");
    let other_repo = temp.path().join("other");
    let elsewhere = temp.path().join("elsewhere");
    fs::create_dir_all(&home).expect("home");
    fs::create_dir_all(&elsewhere).expect("elsewhere");

    init_git_repo(&orbit_repo);
    init_git_repo(&other_repo);

    run_orbit(
        &orbit_repo,
        &home,
        &[
            "init",
            "--non-interactive",
            "--host-name",
            "selector-host",
            "--task-prefix",
            "SEL",
        ],
    )
    .success();
    run_orbit(
        &orbit_repo,
        &home,
        &["workspace", "init", "--name", "orbit"],
    )
    .success();
    run_orbit(
        &other_repo,
        &home,
        &["workspace", "init", "--name", "other"],
    )
    .success();

    let created = run_orbit_json(
        &orbit_repo,
        &home,
        &[
            "task",
            "add",
            "--title",
            "Globally addressable task",
            "--description",
            "Reachable by ID from anywhere",
            "--complexity",
            "low",
            "--json",
        ],
    );
    let task_id = created["id"].as_str().expect("created id").to_string();
    let show_input = format!(r#"{{"id":"{task_id}","model":"codex"}}"#);

    for cwd in [&other_repo, &elsewhere] {
        let shown = run_orbit_json(
            cwd,
            &home,
            &["tool", "run", "orbit.task.show", "--input", &show_input],
        );
        assert_eq!(
            shown["id"],
            Value::String(task_id.clone()),
            "tool run task show from {} must follow the id: {shown}",
            cwd.display()
        );
        assert_eq!(shown["workspace"]["name"], "orbit");
        assert_eq!(shown["workspace"]["id"], "ws_orbit");
    }

    let filtered_input = format!(r#"{{"id":"{task_id}","workspace":"other","model":"codex"}}"#);
    let missed = run_orbit(
        &elsewhere,
        &home,
        &["tool", "run", "orbit.task.show", "--input", &filtered_input],
    )
    .failure();
    let missed_stdout = String::from_utf8_lossy(&missed.get_output().stdout);
    assert!(
        !missed_stdout.contains("Globally addressable task"),
        "a foreign task must not be printed under workspace other: {missed_stdout}"
    );
    let missed_stderr = String::from_utf8_lossy(&missed.get_output().stderr);
    assert!(
        missed_stderr.contains(&task_id),
        "the miss must name the task it looked for: {missed_stderr}"
    );

    let invalid = run_orbit(
        &elsewhere,
        &home,
        &[
            "tool",
            "run",
            "orbit.task.show",
            "--input",
            &format!(r#"{{"id":"{task_id}","workspace":"no-such-workspace","model":"codex"}}"#),
        ],
    )
    .failure();
    let invalid_stderr = String::from_utf8_lossy(&invalid.get_output().stderr);
    assert!(
        invalid_stderr.contains("no-such-workspace"),
        "an invalid explicit selector must be named: {invalid_stderr}"
    );
}

fn task_ids(value: &Value) -> Vec<String> {
    let items = value
        .as_array()
        .cloned()
        .or_else(|| value.get("tasks").and_then(Value::as_array).cloned())
        .unwrap_or_default();
    items
        .iter()
        .filter_map(|task| {
            task.get("id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect()
}

fn run_orbit(cwd: &Path, home: &Path, args: &[&str]) -> assert_cmd::assert::Assert {
    let mut command = cargo_bin_cmd!("orbit");
    // ORB-11300: this fixture exercises selector resolution, so the inherited
    // `ORBIT_WORKSPACE`/`ORBIT_REGISTRY_ROOT` pair is exactly the input under
    // test. Take the whole shared list rather than a local subset.
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .args(args);
    command.assert()
}

fn run_orbit_json(cwd: &Path, home: &Path, args: &[&str]) -> Value {
    let assert = run_orbit(cwd, home, args).success();
    serde_json::from_slice(&assert.get_output().stdout).expect("orbit json output")
}

fn init_git_repo(repo: &Path) {
    fs::create_dir_all(repo).expect("create repo");
    run_git(repo, &["init"]);
    run_git(repo, &["config", "user.name", "Orbit Test"]);
    run_git(repo, &["config", "user.email", "orbit-test@example.com"]);
    run_git(repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("README.md"), "# repo\n").expect("write readme");
    run_git(repo, &["add", "README.md"]);
    run_git(repo, &["commit", "-m", "initial"]);
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
