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

#[test]
fn task_grants_are_visible_beside_requirements_and_in_history() {
    let workspace = workspace();
    let output = workspace.run_as_operator(
        &[
            "task",
            "add",
            "--title",
            "Grant visibility",
            "--complexity",
            "low",
            "--required-tools",
            "github.run.list",
            "--granted-tools",
            "orbit.auto_task.update",
            "--json",
        ],
        "create a granted task",
    );
    let task: Value = serde_json::from_slice(&output.stdout).unwrap();
    let id = task["id"].as_str().unwrap();
    for args in [
        vec!["task", "list", "--format", "table"],
        vec!["task", "list", "--full", "--format", "table"],
    ] {
        let output = workspace.run(&args, "list granted task");
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains("github.run.list"), "{text}");
        assert!(text.contains("orbit.auto_task.update"), "{text}");
        assert!(text.contains("REQUIRED TOOLS"), "{text}");
        assert!(text.contains("GRANTED TOOLS"), "{text}");
    }
    let shown = workspace.run(&["task", "show", id], "show granted task");
    let text = String::from_utf8_lossy(&shown.stdout);
    assert!(text.contains("Required Tools: github.run.list"), "{text}");
    assert!(
        text.contains("Granted Tools: orbit.auto_task.update"),
        "{text}"
    );
    let history = workspace.run(
        &["task", "show", id, "--fields", "history"],
        "show grant history",
    );
    let text = String::from_utf8_lossy(&history.stdout);
    let grant = task["history"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["event"] == "granted_tools_changed")
        .unwrap();
    assert!(text.contains(grant["by"].as_str().unwrap()), "{text}");
    assert!(
        text.contains(&grant["at"].as_str().unwrap()[..19]),
        "{text}"
    );
    assert!(text.contains("orbit.auto_task.update"), "{text}");

    let mut denied = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        denied.env_remove(name);
    });
    let output = denied
        .current_dir(&workspace.work)
        .env("HOME", &workspace.home)
        .env("USERPROFILE", &workspace.home)
        .env("ORBIT_TASK_ACTOR_KIND", "agent")
        .env("ORBIT_ACTIVITY_TOOL_POLICY", "deny")
        .env("ORBIT_ACTIVITY_TOOLS_DENY", "orbit.auto_task.update")
        .env("ORBIT_ACTIVITY_NAME", "agent_implement")
        .args([
            "tool",
            "run",
            "orbit.auto_task.update",
            "--input",
            r#"{"name":"never-applied"}"#,
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("disallow list"),
        "{output:?}"
    );
    let audit = workspace.run(
        &["audit", "list", "--kind", "operation", "--json"],
        "query authorization audit",
    );
    let events: Value = serde_json::from_slice(&audit.stdout).unwrap();
    assert!(
        events
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["command"] == "authorization"
                && event["target_id"] == "orbit.auto_task.update"
                && event["status"] == "denied"),
        "{events}"
    );
    let stats = workspace.run(
        &[
            "audit",
            "stats",
            "--tool",
            "orbit.auto_task.update",
            "--json",
        ],
        "count denied call once",
    );
    let stats: Value = serde_json::from_slice(&stats.stdout).unwrap();
    assert_eq!(stats["policy_denied_count"], 1, "{stats}");
}

#[test]
fn fresh_root_task_list_has_no_startup_warnings() {
    let workspace = workspace();
    let output = workspace.run(&["task", "list", "--json"], "list tasks in a fresh root");
    let tasks: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(tasks.as_array().unwrap().is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("WARN"),
        "a fresh root must load shipped activities without operator warnings: {stderr}"
    );
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

/// Origin is a closed, optional enum: add and update accept each value and
/// refuse anything else with the allowed list, an interactive filing without
/// one stays unset (as every task filed before origins existed reads back),
/// a sweep finding records automation, and `--origin` filters on it.
#[test]
fn task_origin_is_recorded_validated_and_filtered() {
    let workspace = workspace();
    let add = |title: &str, extra: &[&str]| -> Value {
        let mut args = vec![
            "task",
            "add",
            "--title",
            title,
            "--complexity",
            "low",
            "--json",
        ];
        args.extend_from_slice(extra);
        serde_json::from_slice(&workspace.run(&args, title).stdout).unwrap()
    };
    let asked = add("Operator asked", &["--origin", "operator_request"]);
    let decided = add(
        "Orchestrator decided",
        &["--origin", "orchestrator_initiative"],
    );
    let unstated = add("Origin unstated", &[]);
    let finding = add("Sweep finding", &["--tag", "qa-sweep"]);
    assert_eq!(asked["origin"], "operator_request");
    assert_eq!(decided["origin"], "orchestrator_initiative");
    assert_eq!(
        unstated["origin"],
        Value::Null,
        "an interactive filing without an origin must stay unset"
    );
    assert_eq!(
        finding["origin"], "automation",
        "a sweep finding is filed by automation"
    );

    // The tool surface refuses an unknown value and names the allowed set.
    let refused = workspace.run_raw(&[
        "tool",
        "run",
        "orbit.task.add",
        "--input",
        r#"{"title":"Bad origin","description":"d","complexity":"low","origin":"human","workspace":"task-list-test"}"#,
    ]);
    let refusal = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        !refused.status.success(),
        "unknown origin accepted: {refusal}"
    );
    for allowed in ["operator_request", "orchestrator_initiative", "automation"] {
        assert!(
            refusal.contains(allowed),
            "refusal must list `{allowed}`: {refusal}"
        );
    }
    let unstated_id = unstated["id"].as_str().unwrap();
    let rejected_update = workspace.run_raw(&["task", "update", unstated_id, "--origin", "human"]);
    assert!(!rejected_update.status.success());
    let shown = workspace.run(
        &["task", "show", unstated_id, "--fields", "origin", "--json"],
        "show unset origin",
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&shown.stdout).unwrap(),
        Value::Null,
        "a refused origin must leave the task unset"
    );

    workspace.run(
        &[
            "task",
            "update",
            unstated_id,
            "--origin",
            "operator_request",
            "--json",
        ],
        "set origin on update",
    );
    let listed = workspace.run(
        &["task", "list", "--origin", "operator_request", "--json"],
        "filter by origin",
    );
    let mut ids = serde_json::from_slice::<Value>(&listed.stdout)
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|task| task["id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    ids.sort();
    let mut expected = vec![
        asked["id"].as_str().unwrap().to_string(),
        unstated_id.to_string(),
    ];
    expected.sort();
    assert_eq!(ids, expected);
    let automated = workspace.run(
        &["task", "list", "--origin", "automation", "--json"],
        "filter automation",
    );
    let automated: Value = serde_json::from_slice(&automated.stdout).unwrap();
    assert_eq!(automated.as_array().unwrap().len(), 1);
    assert_eq!(automated[0]["id"], finding["id"]);
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

#[test]
fn scheduled_list_orders_future_tasks_and_labels_proposed() {
    let workspace = workspace();
    let unscheduled = workspace.add_task("Unscheduled");
    let early = workspace.add_task("Earlier proposed");
    let late = workspace.add_task("Later");
    let past = workspace.add_task("Past");
    for (id, time) in [
        (&late, "2099-02-02T10:00:00Z"),
        (&early, "2099-02-01T10:00:00Z"),
        (&past, "2000-01-01T00:00:00Z"),
    ] {
        workspace.run(
            &["task", "update", id, "--not-before", time],
            "set schedule",
        );
    }
    let output = workspace.run(
        &["task", "list", "--scheduled", "--json", "--limit", "1"],
        "list next scheduled task",
    );
    let tasks: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(tasks.as_array().unwrap().len(), 1);
    assert_eq!(tasks[0]["id"], early);
    assert_eq!(tasks[0]["status"], "proposed");
    assert_eq!(tasks[0]["not_before"], "2099-02-01T10:00:00Z");
    let output = workspace.run(
        &["task", "list", "--scheduled", "--format", "table"],
        "show scheduled tasks",
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.find(&early).unwrap() < text.find(&late).unwrap(),
        "{text}"
    );
    assert!(
        text.contains("proposed") && text.contains("2099-02-01"),
        "{text}"
    );
    assert!(
        !text.contains(&past) && !text.contains(&unscheduled),
        "{text}"
    );

    let shown = workspace.run(&["task", "show", &early], "show scheduled time");
    let shown = String::from_utf8_lossy(&shown.stdout);
    assert!(
        shown.contains("Scheduled:") && shown.contains("2099-02-01"),
        "{shown}"
    );
    workspace.run_as_operator(
        &[
            "task",
            "update",
            &early,
            "--status",
            "in-progress",
            "--plan",
            "Inspect the scheduled delivery surfaces.",
        ],
        "record manual start",
    );
    let shown = workspace.run(
        &[
            "task",
            "show",
            &early,
            "--fields",
            "not_before,started_at,scheduled_hold",
            "--json",
        ],
        "project scheduled and actual times",
    );
    let shown: Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(shown["not_before"], "2099-02-01T10:00:00Z");
    assert!(shown["started_at"].as_str().is_some(), "{shown}");
    assert_eq!(
        shown["scheduled_hold"], false,
        "an active task is no longer held by its schedule"
    );
    let text = workspace.run(&["task", "show", &early], "show actual start");
    assert!(String::from_utf8_lossy(&text.stdout).contains("Actual start:"));
}

#[test]
fn unknown_task_status_refusal_names_every_valid_status() {
    let workspace = workspace();
    // ORB-20838: agents guess statuses such as `open` or `todo`. The refusal must
    // list the valid statuses so the agent can correct itself, and the guessed
    // alias must still be refused rather than accepted.
    const VALID_STATUSES: [&str; 9] = [
        "proposed",
        "backlog",
        "in-progress",
        "review",
        "done",
        "blocked",
        "archived",
        "rejected",
        "someday",
    ];
    for guessed in ["open", "todo", "cancelled"] {
        let input = format!(r#"{{"status":"{guessed}","workspace":"task-list-test"}}"#);
        let refused = workspace.run_raw(&["tool", "run", "orbit.task.list", "--input", &input]);
        let refusal = format!(
            "{}{}",
            String::from_utf8_lossy(&refused.stdout),
            String::from_utf8_lossy(&refused.stderr)
        );
        assert!(
            !refused.status.success(),
            "unknown status `{guessed}` accepted: {refusal}"
        );
        for valid in VALID_STATUSES {
            assert!(
                refusal.contains(valid),
                "refusal for `{guessed}` must list `{valid}`: {refusal}"
            );
        }
    }
}
