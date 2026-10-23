#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! End-to-end coverage for the borderless list contract (ADR-0307,
//! `docs/design/terminal-interface/specs/table-rendering.md`): a rendered list
//! is exactly one line per record, carries no box-drawing glyphs, and puts its
//! empty state on stderr.
//!
//! The task-list case carries titles far longer than a terminal line, which
//! is what used to wrap a single record across three or four lines. Tool and
//! skill lists share that renderer; their plain and JSON bytes stay pinned by
//! the output goldens.
//!
//! `assert_cmd` captures stdout through a pipe, so the default form these
//! subprocesses produce is the *plain* one: `auto` resolving against a
//! non-terminal sink, which suppresses the header (`specs/output-modes.md` §2).
//! Since [ORB-10586] each case asserts both that form and the header-bearing
//! `--format table`, which is the only way to reach the table rendering from a
//! pipe — and the one-line-per-record invariant is the same in both.

use std::process::Output;

use serde_json::Value;

const BOX_GLYPHS: &[char] = &['─', '│', '┌', '┐', '└', '┘', '├', '┤', '┬', '┴', '┼'];

const LONG_TITLE: &str = "Terminal tables are borderless with a one-line row invariant, and this title is deliberately far longer than any terminal is wide so that it has to be truncated rather than wrapped";

#[test]
fn task_list_renders_one_line_per_task() {
    let workspace = workspace();
    workspace.seed_tasks(
        &(0..3)
            .map(|i| workspace.task_seed(&format!("{LONG_TITLE} ({i})")))
            .collect::<Vec<_>>(),
    );

    assert_both_forms(&workspace, &["task", "list"], 3, "orbit task list");
}

/// The CLI exposes a crew-only OS limit in human and projected readouts.
#[test]
fn task_effective_os_readout_and_filter_use_the_assigned_executor() {
    let workspace = workspace();
    let tasks = workspace.seed_tasks(&[workspace.task_seed("Crew constrained readout")]);
    let id = tasks[0]["id"].as_str().unwrap();
    let config_path = workspace.work.join(".orbit/config.toml");
    let config = if config_path.exists() {
        std::fs::read_to_string(&config_path).unwrap()
    } else {
        String::new()
    };
    std::fs::write(
        config_path,
        format!("{config}\n[crews.readout]\nmodel = \"fixture\"\nprovider = \"readout\"\n"),
    )
    .unwrap();
    let executors = workspace.home.join(".orbit/resources/executors");
    std::fs::create_dir_all(&executors).unwrap();
    std::fs::write(executors.join("readout.yaml"),
        "schemaVersion: 2\nkind: Executor\nmetadata:\n  name: readout\nspec:\n  executor_type: direct_agent\n  command: readout\n  host_os: [macos]\n").unwrap();
    workspace.run(
        &[
            "tool",
            "run",
            "orbit.task.update",
            "--input",
            &serde_json::json!({"id": id, "crew": "readout", "model": "codex"}).to_string(),
        ],
        "assign constrained crew",
    );
    let shown = workspace.run(&["task", "show", id], "human task show");
    assert!(String::from_utf8_lossy(&shown.stdout).contains("Runs on: macOS hosts (crew readout)"));
    let shown = workspace.run(&["task", "show", id, "--json"], "JSON task show");
    let task: Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(
        task["os_requirement"]["any_of"],
        serde_json::json!(["macos"])
    );
    let field = workspace.run(
        &["task", "show", id, "--fields", "os_requirement", "--json"],
        "projected task show",
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&field.stdout).unwrap(),
        task["os_requirement"]
    );
    for (os, count) in [("macos", 1), ("linux", 0)] {
        let listed = workspace.run(
            &["task", "list", "--runs-on", os, "--json"],
            "effective OS filter",
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&listed.stdout)
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            count
        );
    }
    let listed = workspace.run(&["task", "list", "--format", "table"], "task OS column");
    assert!(String::from_utf8_lossy(&listed.stdout).contains("macOS hosts (crew readout)"));
}

/// A stored title may carry a line feed, carriage return, or tab. The plain form
/// escapes them so the record stays one line with a stable field count, while
/// JSON and NDJSON carry the value exactly as stored (spec §1).
#[test]
fn task_list_plain_escapes_embedded_separators_and_json_keeps_them() {
    const RAW_TITLE: &str = "first line\nsecond\tfield\r\nthird";
    const ESCAPED_TITLE: &str = "first line\\nsecond\\tfield\\r\\nthird";
    const ORDINARY_TITLE: &str = "An ordinary single-line title";
    let workspace = workspace();
    workspace.seed_tasks(&[
        workspace.task_seed(RAW_TITLE),
        workspace.task_seed(ORDINARY_TITLE),
    ]);

    let plain = workspace.run(&["task", "list"], "plain task list");
    let stdout = String::from_utf8(plain.stdout).expect("utf-8 stdout");
    assert!(
        !stdout.contains('\r'),
        "a raw carriage return reached the pipe: {stdout:?}"
    );
    let rows = stdout.lines().collect::<Vec<_>>();
    assert_eq!(rows.len(), 2, "one physical line per task: {stdout:?}");
    let fields = rows
        .iter()
        .map(|row| row.split('\t').collect::<Vec<_>>())
        .collect::<Vec<_>>();
    assert_eq!(
        fields[0].len(),
        fields[1].len(),
        "every row carries the same field count: {stdout:?}"
    );
    let titles = fields
        .iter()
        .flat_map(|row| row.iter().copied())
        .collect::<Vec<_>>();
    assert!(
        titles.contains(&ESCAPED_TITLE),
        "the embedded separators are escaped within one field: {stdout:?}"
    );
    assert!(
        titles.contains(&ORDINARY_TITLE),
        "an ordinary title renders unchanged: {stdout:?}"
    );

    let json = workspace.run(&["task", "list", "--format", "json"], "task list JSON");
    let tasks: Value = serde_json::from_slice(&json.stdout).expect("task list JSON");
    let json_titles = tasks
        .as_array()
        .expect("task array")
        .iter()
        .map(|task| task["title"].as_str().expect("title").to_string())
        .collect::<Vec<_>>();
    assert!(
        json_titles.iter().any(|title| title == RAW_TITLE),
        "JSON keeps the stored title: {json_titles:?}"
    );

    let ndjson = workspace.run(&["task", "list", "--format", "ndjson"], "task list NDJSON");
    let ndjson_titles = String::from_utf8(ndjson.stdout)
        .expect("utf-8 NDJSON")
        .lines()
        .map(|line| {
            let task: Value = serde_json::from_str(line).expect("NDJSON record");
            task["title"].as_str().expect("title").to_string()
        })
        .collect::<Vec<_>>();
    assert_eq!(ndjson_titles.len(), 2, "one NDJSON record per task");
    assert!(
        ndjson_titles.iter().any(|title| title == RAW_TITLE),
        "NDJSON keeps the stored title: {ndjson_titles:?}"
    );
}

#[test]
fn job_show_json_contains_no_human_step_rows() {
    let workspace = workspace();

    let json = workspace.run(
        &["job", "show", "task_pr_pipeline", "--format", "json"],
        "job show JSON",
    );
    let document: Value = serde_json::from_slice(&json.stdout).expect("job show JSON");
    assert!(document.is_object(), "job show returned {document}");
    assert!(
        !String::from_utf8_lossy(&json.stdout).contains("ID:"),
        "JSON output contained human step rows: {}",
        String::from_utf8_lossy(&json.stdout)
    );

    let human = workspace.run(
        &["job", "show", "task_pr_pipeline"],
        "job show human output",
    );
    let human_stdout = String::from_utf8_lossy(&human.stdout);
    assert!(human_stdout.contains("Steps:"), "{human_stdout}");
    assert!(human_stdout.contains("ID:"), "{human_stdout}");
}

#[test]
fn a_list_with_no_matches_leaves_stdout_empty_and_explains_itself_on_stderr() {
    let workspace = workspace();
    workspace.add_task(LONG_TITLE);

    // Every task starts in `proposed`, so a `done` filter matches nothing.
    let empty = workspace.run(&["task", "list", "--status", "done"], "empty task list");

    assert!(
        empty.stdout.is_empty(),
        "a consumer piping the command receives an empty stream, not prose: {}",
        String::from_utf8_lossy(&empty.stdout)
    );
    assert!(
        !String::from_utf8_lossy(&empty.stderr).trim().is_empty(),
        "the empty state names what was searched, on stderr"
    );
}

/// Assert the one-record-per-line invariant plus the absence of borders, for
/// both renderings of a list: plain (no header) and `--format table` (one).
fn assert_both_forms(workspace: &TestWorkspace, args: &[&str], records: usize, label: &str) {
    let plain = workspace.run(args, label);
    assert_record_lines(&plain, records, false, &format!("{label} (plain)"));

    let mut table_args = args.to_vec();
    table_args.extend(["--format", "table"]);
    let table = workspace.run(&table_args, label);
    assert_record_lines(&table, records, true, &format!("{label} --format table"));
}

/// Assert the one-record-per-line invariant plus the absence of borders.
fn assert_record_lines(output: &Output, records: usize, header: bool, label: &str) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines = stdout.lines().collect::<Vec<_>>();

    assert!(
        !stdout.chars().any(|c| BOX_GLYPHS.contains(&c)),
        "{label} must not draw box rules:\n{stdout}"
    );
    let expected = records + usize::from(header);
    assert_eq!(
        lines.len(),
        expected,
        "{label} renders {records} records as {expected} lines:\n{stdout}"
    );
    assert!(
        lines.iter().all(|line| !line.starts_with(' ')),
        "{label} indents no line:\n{stdout}"
    );
    if !header {
        assert!(
            lines.iter().all(|line| line.contains('\t')) || records == 0,
            "{label} is the plain form, so fields are tab-separated:\n{stdout}"
        );
    }
}

use crate::task_workspace::{TaskSeed, TestWorkspace};

fn workspace() -> TestWorkspace {
    let mut template = TaskSeed::new("");
    template.description = "A record whose rendered row must stay on one line.".into();
    TestWorkspace::new("table-rendering-test", template)
}

#[test]
fn non_tty_sink_has_zero_width_and_no_color_however_loudly_the_env_claims_otherwise() {
    use assert_cmd::cargo::cargo_bin_cmd;
    use orbit_common::test_env;

    let workspace = workspace();
    workspace.add_task(LONG_TITLE);
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let output = command
        .current_dir(&workspace.work)
        .env("HOME", &workspace.home)
        .env("USERPROFILE", &workspace.home)
        .env("COLUMNS", "200")
        .env("CLICOLOR_FORCE", "1")
        .env_remove("ORBIT_FORMAT")
        .args(["task", "list", "--format", "table"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains('\x1b'),
        "a redirected stream is never styled, even with CLICOLOR_FORCE set"
    );
    assert!(
        stdout.contains(LONG_TITLE),
        "a redirected stream is never width-adapted, even with COLUMNS set: {stdout}"
    );
    assert_record_lines(&output, 1, true, "non-TTY table");
}

#[test]
fn workspace_list_obeys_format_plain_and_header_and_table_and_ndjson() {
    let workspace = workspace();

    // 1. Plain without header: 1 record, tab-separated fields, NO header row.
    let plain = workspace.run(
        &["workspace", "list", "--format", "plain"],
        "workspace list plain",
    );
    let stdout = String::from_utf8(plain.stdout).expect("utf-8 stdout");
    let lines = stdout.lines().collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        1,
        "exactly one line for one workspace in plain mode: {stdout:?}"
    );
    let fields = lines[0].split('\t').collect::<Vec<_>>();
    assert!(
        fields.len() >= 7,
        "tab-separated fields in plain row: {stdout:?}"
    );
    assert!(
        !lines[0].starts_with("NAME\t"),
        "no header row without --header: {stdout:?}"
    );

    // 2. Plain with --header: header row first, tab-separated fields.
    let plain_header = workspace.run(
        &["workspace", "list", "--format", "plain", "--header"],
        "workspace list plain --header",
    );
    let stdout_header = String::from_utf8(plain_header.stdout).expect("utf-8 stdout");
    let lines_header = stdout_header.lines().collect::<Vec<_>>();
    assert_eq!(
        lines_header.len(),
        2,
        "header row plus one data row: {stdout_header:?}"
    );
    assert!(
        lines_header[0].contains('\t'),
        "header row is tab-separated: {stdout_header:?}"
    );
    assert!(
        lines_header[1].contains('\t'),
        "data row is tab-separated: {stdout_header:?}"
    );
    assert_eq!(
        lines_header[0].split('\t').count(),
        lines_header[1].split('\t').count(),
        "header and data rows have same field count: {stdout_header:?}"
    );

    // 3. Table: aligned columns with header, exactly 2 lines.
    let table = workspace.run(
        &["workspace", "list", "--format", "table"],
        "workspace list table",
    );
    let stdout_table = String::from_utf8(table.stdout).expect("utf-8 stdout");
    let lines_table = stdout_table.lines().collect::<Vec<_>>();
    assert_eq!(
        lines_table.len(),
        2,
        "header row plus one data row in table: {stdout_table:?}"
    );
    assert!(
        !stdout_table.chars().any(|c| BOX_GLYPHS.contains(&c)),
        "table must not draw box rules: {stdout_table}"
    );

    // 4. NDJSON: one line per workspace, valid JSON document.
    let ndjson = workspace.run(
        &["workspace", "list", "--format", "ndjson"],
        "workspace list ndjson",
    );
    let stdout_ndjson = String::from_utf8(ndjson.stdout).expect("utf-8 stdout");
    let lines_ndjson = stdout_ndjson.lines().collect::<Vec<_>>();
    assert_eq!(
        lines_ndjson.len(),
        1,
        "one line per workspace: {stdout_ndjson:?}"
    );
    let doc: Value = serde_json::from_str(lines_ndjson[0]).expect("valid JSON line");
    assert!(doc.is_object());
    assert!(doc.get("id").is_some());
    assert!(doc.get("name").is_some());
    assert!(doc.get("status").is_some());

    // 5. JSON: array of workspace objects preserving current fields.
    let json = workspace.run(
        &["workspace", "list", "--format", "json"],
        "workspace list json",
    );
    let doc_json: Value = serde_json::from_slice(&json.stdout).expect("valid JSON array");
    let array = doc_json.as_array().expect("array");
    assert_eq!(array.len(), 1);
    assert!(array[0].get("id").is_some());
    assert!(array[0].get("name").is_some());
    assert!(array[0].get("status").is_some());
}

#[test]
fn config_keys_obeys_format_plain_and_header_and_table_and_ndjson() {
    let workspace = workspace();

    // 1. JSON document keeps current {"keys": [...]} shape and fields
    let json = workspace.run(&["config", "keys", "--format", "json"], "config keys json");
    let doc_json: Value = serde_json::from_slice(&json.stdout).expect("valid JSON object");
    let keys = doc_json["keys"].as_array().expect("keys array");
    assert!(!keys.is_empty(), "registry has keys");
    let key_count = keys.len();
    assert!(keys[0].get("key").is_some());
    assert!(keys[0].get("type").is_some());
    assert!(keys[0].get("section").is_some());
    assert!(keys[0].get("description").is_some());

    // 2. Plain without header: key_count rows, all tab-separated, no header row.
    let plain = workspace.run(
        &["config", "keys", "--format", "plain"],
        "config keys plain",
    );
    let stdout = String::from_utf8(plain.stdout).expect("utf-8 stdout");
    let lines = stdout.lines().collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        key_count,
        "exactly one line per key: {stdout:?}"
    );
    for line in &lines {
        let fields = line.split('\t').collect::<Vec<_>>();
        assert_eq!(fields.len(), 4, "4 tab-separated fields per row: {line}");
    }
    assert!(
        !lines[0].starts_with("KEY\t"),
        "no header without --header: {stdout:?}"
    );

    // 3. Plain with --header: header row first, key_count + 1 lines.
    let plain_header = workspace.run(
        &["config", "keys", "--format", "plain", "--header"],
        "config keys plain --header",
    );
    let stdout_header = String::from_utf8(plain_header.stdout).expect("utf-8 stdout");
    let lines_header = stdout_header.lines().collect::<Vec<_>>();
    assert_eq!(
        lines_header.len(),
        key_count + 1,
        "header row plus keys: {stdout_header:?}"
    );
    for line in &lines_header {
        let fields = line.split('\t').collect::<Vec<_>>();
        assert_eq!(
            fields.len(),
            4,
            "4 tab-separated fields in each row: {line}"
        );
    }
    assert_eq!(
        lines_header[0].split('\t').count(),
        4,
        "header has 4 fields"
    );

    // 4. Table: aligned columns with header, key_count + 1 lines.
    let table = workspace.run(
        &["config", "keys", "--format", "table"],
        "config keys table",
    );
    let stdout_table = String::from_utf8(table.stdout).expect("utf-8 stdout");
    let lines_table = stdout_table.lines().collect::<Vec<_>>();
    assert_eq!(
        lines_table.len(),
        key_count + 1,
        "header plus keys in table: {stdout_table:?}"
    );
    assert!(
        !stdout_table.chars().any(|c| BOX_GLYPHS.contains(&c)),
        "table must not draw box rules: {stdout_table}"
    );

    // 5. NDJSON: one line per key, key_count lines, each a valid JSON document.
    let ndjson = workspace.run(
        &["config", "keys", "--format", "ndjson"],
        "config keys ndjson",
    );
    let stdout_ndjson = String::from_utf8(ndjson.stdout).expect("utf-8 stdout");
    let lines_ndjson = stdout_ndjson.lines().collect::<Vec<_>>();
    assert_eq!(
        lines_ndjson.len(),
        key_count,
        "one NDJSON line per key: {stdout_ndjson:?}"
    );
    for line in lines_ndjson {
        let doc: Value = serde_json::from_str(line).expect("valid JSON line");
        assert!(doc.is_object());
        assert!(doc.get("key").is_some());
        assert!(doc.get("type").is_some());
        assert!(doc.get("section").is_some());
        assert!(doc.get("description").is_some());
    }
}
