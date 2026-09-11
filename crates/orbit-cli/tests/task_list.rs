#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::Path;
use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::{TempDir, tempdir};

struct TestWorkspace {
    _temp: TempDir,
    home: std::path::PathBuf,
    work: std::path::PathBuf,
}

impl TestWorkspace {
    fn new() -> Self {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let work = home.join("work");
        fs::create_dir_all(&home).expect("create home");
        fs::create_dir_all(work.join(".git")).expect("create work repo");

        let workspace = Self {
            _temp: temp,
            home,
            work,
        };
        workspace.run(
            &["workspace", "init", "--name", "task-list-test"],
            "initialize workspace",
        );
        workspace
    }

    fn add_task(&self, title: &str) -> String {
        let output = self.run(
            &[
                "task",
                "add",
                "--title",
                title,
                "--description",
                "Task list test item.",
                "--complexity",
                "low",
                "--json",
            ],
            "add task",
        );
        let val: Value = serde_json::from_slice(&output.stdout).expect("task add JSON");
        val["id"].as_str().expect("task id").to_string()
    }

    fn add_task_with_tag(&self, title: &str, tag: &str) -> String {
        let output = self.run(
            &[
                "task",
                "add",
                "--title",
                title,
                "--description",
                "Task list test item with tag.",
                "--tag",
                tag,
                "--complexity",
                "low",
                "--json",
            ],
            "add tagged task",
        );
        let val: Value = serde_json::from_slice(&output.stdout).expect("task add JSON");
        val["id"].as_str().expect("task id").to_string()
    }

    fn update_status(&self, id: &str, status: &str) {
        self.run(
            &["task", "update", id, "--status", status, "--json"],
            "update task status",
        );
    }

    fn run(&self, args: &[&str], label: &str) -> Output {
        let output = run_orbit(&self.work, &self.home, args);
        assert!(
            output.status.success(),
            "{label} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
}

fn run_orbit(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .args(args);
    command.output().expect("run orbit")
}

#[test]
fn task_list_truncation_notice_and_bare_json_array_with_60_tasks() {
    let workspace = TestWorkspace::new();
    for i in 0..60 {
        workspace.add_task(&format!("Task {i:02}"));
    }

    // Default limit (50) on 60 tasks emits a truncation notice to stderr. All
    // 60 tasks are non-terminal (`proposed`), so the default status-aware
    // listing is a single bucket here, but the notice still states the
    // general rule rather than a blanket "newest first" (ORB-12200).
    let output = workspace.run(&["task", "list"], "default task list");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains(
            "showing 50 of 60 tasks (non-terminal tasks first, then terminal, each newest first); use --limit N or a filter to see more"
        ),
        "stderr must contain truncation notice:\n{stderr}"
    );
    assert!(
        !stdout.contains("showing 50 of 60 tasks"),
        "stdout must not contain truncation notice:\n{stdout}"
    );

    // Default JSON stays the bare array the frozen `--json` contract pins.
    let json_output = workspace.run(&["task", "list", "--json"], "task list --json");
    let tasks: Value = serde_json::from_slice(&json_output.stdout).expect("task list json payload");
    assert_eq!(tasks.as_array().expect("tasks array").len(), 50);

    // Raising --limit to 100 shows all 60 tasks.
    let full_json = workspace.run(
        &["task", "list", "--limit", "100", "--json"],
        "task list --limit 100 --json",
    );
    let full_tasks: Value = serde_json::from_slice(&full_json.stdout).expect("full json payload");
    assert_eq!(full_tasks.as_array().expect("tasks array").len(), 60);

    // Raising --limit to 60 emits no notice on stderr
    let unconstrained = workspace.run(&["task", "list", "--limit", "60"], "task list --limit 60");
    let unconstrained_stderr = String::from_utf8_lossy(&unconstrained.stderr);
    assert!(
        !unconstrained_stderr.contains("showing"),
        "unconstrained listing should not emit truncation notice:\n{unconstrained_stderr}"
    );
}

#[test]
fn older_someday_task_discoverable_ahead_of_50_newer_done_tasks() {
    let workspace = TestWorkspace::new();

    // Create an older task and place it in someday
    let someday_id = workspace.add_task("Older someday priority task");
    workspace.update_status(&someday_id, "someday");

    // Create 50 newer tasks and mark them done
    for i in 0..50 {
        let id = workspace.add_task(&format!("Done task {i:02}"));
        workspace.update_status(&id, "done");
    }

    // Default listing with limit 50 must still discover the older someday task
    let output = workspace.run(&["task", "list"], "task list with someday");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Older someday priority task"),
        "older someday task must be in default listing:\n{stdout}"
    );

    // The truncation notice states the real status-aware rule rather than a
    // blanket "newest first": the someday task below is older than every done
    // task, yet it is shown first (ORB-12200).
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(
            "showing 50 of 51 tasks (non-terminal tasks first, then terminal, each newest first); use --limit N or a filter to see more"
        ),
        "stderr must contain truncation notice:\n{stderr}"
    );

    // In JSON output, the someday task is listed first ahead of done tasks
    let json_output = workspace.run(&["task", "list", "--json"], "task list JSON");
    let payload: Value =
        serde_json::from_slice(&json_output.stdout).expect("task list json payload");
    let tasks = payload.as_array().expect("tasks array");
    assert_eq!(tasks.len(), 50);
    assert_eq!(tasks[0]["id"], someday_id);
    assert_eq!(tasks[0]["status"], "someday");
    assert_eq!(tasks[0]["title"], "Older someday priority task");

    // The remaining 49 items are the newest done tasks
    for task in &tasks[1..] {
        assert_eq!(task["status"], "done");
    }
}

#[test]
fn task_list_help_documents_rule() {
    let workspace = TestWorkspace::new();
    let output = workspace.run(&["task", "list", "--help"], "task list --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("status-aware rule"),
        "help must mention status-aware rule:\n{stdout}"
    );
    assert!(
        stdout.contains("non-terminal"),
        "help must mention non-terminal tasks:\n{stdout}"
    );
}

#[test]
fn existing_filters_keep_behaviour() {
    let workspace = TestWorkspace::new();

    let id1 = workspace.add_task_with_tag("Task with tag alpha", "alpha");
    let id2 = workspace.add_task_with_tag("Task with tag beta", "beta");
    workspace.update_status(&id2, "done");

    // --status filter
    let status_filter = workspace.run(
        &["task", "list", "--status", "done", "--json"],
        "filter status done",
    );
    let status_payload: Value = serde_json::from_slice(&status_filter.stdout).expect("status json");
    let status_tasks = status_payload.as_array().expect("tasks array");
    assert_eq!(status_tasks.len(), 1);
    assert_eq!(status_tasks[0]["id"], id2);

    // --tag filter
    let tag_filter = workspace.run(
        &["task", "list", "--tag", "alpha", "--json"],
        "filter tag alpha",
    );
    let tag_payload: Value = serde_json::from_slice(&tag_filter.stdout).expect("tag json");
    let tag_tasks = tag_payload.as_array().expect("tasks array");
    assert_eq!(tag_tasks.len(), 1);
    assert_eq!(tag_tasks[0]["id"], id1);

    // --limit filter
    let limit_filter = workspace.run(
        &["task", "list", "--limit", "1", "--json"],
        "filter limit 1",
    );
    let limit_payload: Value = serde_json::from_slice(&limit_filter.stdout).expect("limit json");
    let limit_tasks = limit_payload.as_array().expect("tasks array");
    assert_eq!(limit_tasks.len(), 1);
}
