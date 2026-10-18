#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;

use crate::task_workspace::{TaskSeed, TestWorkspace};
use orbit_types::task::TaskStatus;

fn workspace() -> TestWorkspace {
    let mut template = TaskSeed::new("");
    template.description = "Task list test item.".into();
    TestWorkspace::new("task-list-test", template)
}

fn completed_add_and_update_leave_the_next_write_free_list_indexed() {
    let workspace = workspace();
    let added = workspace.add_task_cli_with_tags("Fresh index", &["initial"]);
    let id = added["id"].as_str().unwrap().to_string();
    for (tag, expected_title) in [("initial", "Fresh index"), ("updated", "Updated index")] {
        if tag == "updated" {
            workspace.run(
                &[
                    "task",
                    "update",
                    &id,
                    "--title",
                    expected_title,
                    "--tag",
                    tag,
                    "--comment",
                    "Published update",
                    "--json",
                ],
                "update task and history",
            );
        }
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        let output = command
            .current_dir(&workspace.work)
            .env("HOME", &workspace.home)
            .env("USERPROFILE", &workspace.home)
            .env("RUST_LOG", "warn,orbit.store.task_query=debug")
            .env("NO_COLOR", "1")
            .args(["task", "list", "--tag", tag, "--json"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let tasks: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(tasks.as_array().unwrap().len(), 1);
        assert_eq!(tasks[0]["id"], id);
        assert_eq!(tasks[0]["title"], expected_title);
        let logs = String::from_utf8_lossy(&output.stderr);
        assert!(
            logs.contains("path=\"index\""),
            "next write-free list must accept the generated index: {logs}"
        );
        assert!(
            !logs.contains("bundle_scan"),
            "completed writes must not require a bundle scan: {logs}"
        );
        assert!(
            !logs.contains("WARN"),
            "ordinary indexed reads must be quiet: {logs}"
        );
    }
}

fn task_list_truncation_notice_and_bare_json_array_with_60_tasks() {
    let workspace = workspace();
    workspace.seed_tasks(
        &(0..60)
            .map(|i| workspace.task_seed(&format!("Task {i:02}")))
            .collect::<Vec<_>>(),
    );

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
fn task_list_truncation_notice_reaches_stderr_in_json_and_ndjson_modes() {
    let workspace = workspace();
    workspace.seed_tasks(
        &(0..3)
            .map(|i| workspace.task_seed(&format!("Task {i:02}")))
            .collect::<Vec<_>>(),
    );
    let truncation_notice = "showing 2 of 3 tasks (non-terminal tasks first, then terminal, each newest first); use --limit N or a filter to see more";

    let json = workspace.run(
        &["task", "list", "--limit", "2", "--format", "json"],
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
        2,
        "--format json stdout must stay the bare array, untouched by the notice"
    );
    assert!(
        !json_stdout.contains("showing"),
        "stdout must not carry the truncation notice:\n{json_stdout}"
    );

    let ndjson = workspace.run(
        &["task", "list", "--limit", "2", "--format", "ndjson"],
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
        2,
        "--format ndjson stdout must stay one record per line, untouched by the notice:\n{ndjson_stdout}"
    );
    for line in &lines {
        let record: Value = serde_json::from_str(line).expect("ndjson line is a JSON object");
        assert!(record.is_object(), "each ndjson line is one task record");
    }
}

fn older_someday_task_discoverable_ahead_of_newer_done_tasks() {
    let workspace = workspace();

    let mut someday = workspace.task_seed("Older someday priority task");
    someday.status = TaskStatus::Someday;
    let mut seeds = vec![someday];
    for i in 0..3 {
        let mut done = workspace.task_seed(&format!("Done task {i:02}"));
        done.status = TaskStatus::Done;
        seeds.push(done);
    }
    let seeded = workspace.seed_tasks(&seeds);
    let someday_id = seeded[0]["id"].as_str().unwrap();

    // Status-neutral listing with explicit limit 3 must still discover the older someday task
    let output = workspace.run(&["task", "list", "--limit", "3"], "task list with someday");
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
            "showing 3 of 4 tasks (non-terminal tasks first, then terminal, each newest first); use --limit N or a filter to see more"
        ),
        "stderr must contain truncation notice:\n{stderr}"
    );

    // In JSON output, the someday task is listed first ahead of done tasks
    let json_output = workspace.run(
        &["task", "list", "--limit", "3", "--json"],
        "task list JSON",
    );
    let payload: Value =
        serde_json::from_slice(&json_output.stdout).expect("task list json payload");
    let tasks = payload.as_array().expect("tasks array");
    assert_eq!(tasks.len(), 3);
    assert_eq!(tasks[0]["id"], someday_id);
    assert_eq!(tasks[0]["status"], "someday");
    assert_eq!(tasks[0]["title"], "Older someday priority task");

    // The remaining 2 items are the newest done tasks
    for task in &tasks[1..] {
        assert_eq!(task["status"], "done");
    }
}

/// ORB-12206: in human modes (`table` and `plain`), the truncation notice
/// must be emitted after the table body, not before it.
#[test]
fn task_list_truncation_notice_emitted_after_table_body_in_human_modes() {
    let workspace = workspace();
    workspace.seed_tasks(
        &(0..5)
            .map(|i| workspace.task_seed(&format!("Task {i:02}")))
            .collect::<Vec<_>>(),
    );
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

#[test]
fn task_index_and_bounded_listing() {
    for (name, check) in [
        (
            "completed_add_and_update_leave_the_next_write_free_list_indexed",
            completed_add_and_update_leave_the_next_write_free_list_indexed as fn(),
        ),
        (
            "task_list_truncation_notice_and_bare_json_array_with_60_tasks",
            task_list_truncation_notice_and_bare_json_array_with_60_tasks as fn(),
        ),
        (
            "task_list_truncation_notice_reaches_stderr_in_json_and_ndjson_modes",
            task_list_truncation_notice_reaches_stderr_in_json_and_ndjson_modes as fn(),
        ),
        (
            "older_someday_task_discoverable_ahead_of_newer_done_tasks",
            older_someday_task_discoverable_ahead_of_newer_done_tasks as fn(),
        ),
    ] {
        tracing::info!(case = name, "CLI contract row");
        check();
    }
}
