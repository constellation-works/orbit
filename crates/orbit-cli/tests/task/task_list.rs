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

    /// Park a fixture task in an arbitrary status. `--force` is the human
    /// override for the lifecycle table (ORB-12245); listing behaviour, not
    /// the route a task took to its status, is what these tests measure.
    fn update_status(&self, id: &str, status: &str) {
        self.run(
            &[
                "task", "update", id, "--status", status, "--force", "--json",
            ],
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

    fn run_merged(&self, args: &[&str], label: &str) -> String {
        let temp_file = tempfile::NamedTempFile::new().expect("temp file for merged output");
        let stdout_file = temp_file
            .as_file()
            .try_clone()
            .expect("clone file for stdout");
        let stderr_file = temp_file
            .as_file()
            .try_clone()
            .expect("clone file for stderr");
        let mut command = std::process::Command::new(assert_cmd::cargo::cargo_bin!("orbit"));
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&self.work)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .stdout(stdout_file)
            .stderr(stderr_file)
            .args(args);
        let status = command.status().expect("run orbit");
        assert!(status.success(), "{label} failed with status {status:?}");
        fs::read_to_string(temp_file.path()).expect("read merged output")
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

/// ORB-12203: the truncation notice on a machine-readable mode must reach
/// stderr, and stdout must stay exactly the records — a bare array for
/// `--format json`, one record per line for `--format ndjson` — with no
/// notice mixed in.
#[test]
fn task_list_truncation_notice_reaches_stderr_in_json_and_ndjson_modes() {
    let workspace = TestWorkspace::new();
    for i in 0..60 {
        workspace.add_task(&format!("Task {i:02}"));
    }
    let truncation_notice = "showing 50 of 60 tasks (non-terminal tasks first, then terminal, each newest first); use --limit N or a filter to see more";

    let json = workspace.run(
        &["task", "list", "--format", "json"],
        "task list --format json",
    );
    let json_stdout = String::from_utf8_lossy(&json.stdout);
    let json_stderr = String::from_utf8_lossy(&json.stderr);
    assert!(
        json_stderr.contains(truncation_notice),
        "--format json must report truncation on stderr:\n{json_stderr}"
    );
    let tasks: Value = serde_json::from_slice(&json.stdout).expect("bare json array");
    assert_eq!(
        tasks.as_array().expect("tasks array").len(),
        50,
        "--format json stdout must stay the bare array, untouched by the notice"
    );
    assert!(
        !json_stdout.contains("showing"),
        "stdout must not carry the truncation notice:\n{json_stdout}"
    );

    let ndjson = workspace.run(
        &["task", "list", "--format", "ndjson"],
        "task list --format ndjson",
    );
    let ndjson_stdout = String::from_utf8_lossy(&ndjson.stdout);
    let ndjson_stderr = String::from_utf8_lossy(&ndjson.stderr);
    assert!(
        ndjson_stderr.contains(truncation_notice),
        "--format ndjson must report truncation on stderr:\n{ndjson_stderr}"
    );
    let lines: Vec<&str> = ndjson_stdout.lines().collect();
    assert_eq!(
        lines.len(),
        50,
        "--format ndjson stdout must stay one record per line, untouched by the notice:\n{ndjson_stdout}"
    );
    for line in &lines {
        let record: Value = serde_json::from_str(line).expect("ndjson line is a JSON object");
        assert!(record.is_object(), "each ndjson line is one task record");
    }
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

/// ORB-12206: in human modes (`table` and `plain`), the truncation notice
/// must be emitted after the table body, not before it.
#[test]
fn task_list_truncation_notice_emitted_after_table_body_in_human_modes() {
    let workspace = TestWorkspace::new();
    for i in 0..5 {
        workspace.add_task(&format!("Task {i:02}"));
    }
    let truncation_notice = "showing 2 of 5 tasks";

    // 1. Table mode: merged streams must have header and rows before the trailing notice.
    let table_output = workspace.run_merged(
        &["task", "list", "--limit", "2", "--format", "table"],
        "task list --limit 2 --format table",
    );
    let table_header_idx = table_output
        .find("ID")
        .expect("table output must contain ID header");
    let table_last_row_idx = table_output
        .rfind("Task 0")
        .expect("table output must contain task row");
    let table_notice_idx = table_output
        .find(truncation_notice)
        .expect("table output must contain truncation notice");
    assert!(
        table_header_idx < table_notice_idx,
        "table header must appear before truncation notice in table mode, but was below it:\n{table_output}"
    );
    assert!(
        table_last_row_idx < table_notice_idx,
        "table rows must appear before truncation notice in table mode, but were below it:\n{table_output}"
    );

    // 2. Plain mode: non-tty default resolves to plain mode.
    // Merged streams must have records before the trailing notice.
    let plain_output = workspace.run_merged(
        &["task", "list", "--limit", "2"],
        "task list --limit 2 plain",
    );
    let plain_last_record_idx = plain_output
        .rfind("Task 0")
        .expect("plain output must contain task record");
    let plain_notice_idx = plain_output
        .find(truncation_notice)
        .expect("plain output must contain truncation notice");
    assert!(
        plain_last_record_idx < plain_notice_idx,
        "plain records must appear before truncation notice in plain mode, but were below it:\n{plain_output}"
    );

    // 3. Independent streams: stdout contains the table body and stderr contains the notice.
    let table_cmd = workspace.run(
        &["task", "list", "--limit", "2", "--format", "table"],
        "task list --limit 2 --format table streams",
    );
    let table_stdout = String::from_utf8_lossy(&table_cmd.stdout);
    let table_stderr = String::from_utf8_lossy(&table_cmd.stderr);
    assert!(
        table_stdout.contains("ID") && table_stdout.contains("Task 0"),
        "table stdout must contain table body:\n{table_stdout}"
    );
    assert!(
        !table_stdout.contains("showing"),
        "table stdout must not contain truncation notice:\n{table_stdout}"
    );
    assert!(
        table_stderr.contains(truncation_notice),
        "table stderr must contain truncation notice:\n{table_stderr}"
    );

    let plain_cmd = workspace.run(
        &["task", "list", "--limit", "2"],
        "task list --limit 2 plain streams",
    );
    let plain_stdout = String::from_utf8_lossy(&plain_cmd.stdout);
    let plain_stderr = String::from_utf8_lossy(&plain_cmd.stderr);
    assert!(
        plain_stdout.contains("Task 0"),
        "plain stdout must contain records:\n{plain_stdout}"
    );
    assert!(
        !plain_stdout.contains("showing"),
        "plain stdout must not contain truncation notice:\n{plain_stdout}"
    );
    assert!(
        plain_stderr.contains(truncation_notice),
        "plain stderr must contain truncation notice:\n{plain_stderr}"
    );
}
