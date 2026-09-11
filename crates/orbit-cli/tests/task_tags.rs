#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::Path;
use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

#[test]
fn task_cli_roundtrips_filters_and_replaces_tags() {
    let workspace = TestWorkspace::new();
    let perf = workspace.add_task("Perf task", &["perf"]);
    workspace.add_task("Bench task", &["bench"]);
    let both = workspace.add_task("Perf bench task", &["  Perf ", "BENCH"]);

    assert_eq!(both["tags"], json!(["perf", "bench"]));

    // ORB-10310: freshly-added tasks are `proposed`; status-neutral listing must
    // surface them without `--all` or `--status proposed`.
    let perf_list = workspace.run(
        &["task", "list", "--tag", "perf", "--json"],
        None,
        "list perf tasks",
    );
    assert_task_titles(&perf_list, &["Perf task", "Perf bench task"]);

    // ORB-00202: `orbit task search` was deleted; the substring+tag case
    // migrates to `orbit search --kind task --tag <...>`. Results land
    // under `output["results"]` rather than at the top level.
    let both_search = workspace.run(
        &[
            "search",
            "tag-search",
            "--kind",
            "task",
            "--tag",
            "perf",
            "--tag",
            "bench",
            "--json",
        ],
        None,
        "orbit search perf+bench tasks",
    );
    assert_orbit_search_titles(&both_search, &["Perf bench task"]);

    let perf_id = perf["id"].as_str().expect("perf task id");
    let updated = workspace.run(
        &["task", "update", perf_id, "--tag", "docs", "--json"],
        None,
        "replace tags",
    );
    let updated: Value = serde_json::from_slice(&updated.stdout).expect("update JSON");
    assert_eq!(updated["tags"], json!(["docs"]));
}

/// ORB-10310: `orbit task list` is status-neutral and bounded by `--limit`.
#[test]
fn task_list_is_status_neutral_and_bounded_by_limit() {
    let workspace = TestWorkspace::new();
    // Every task starts in `proposed`; the default listing must include them
    // all with no `--status`/`--all`.
    for index in 0..3 {
        workspace.add_task(&format!("Task {index}"), &[]);
    }

    let default_list = workspace.run(&["task", "list", "--json"], None, "default list");
    let default_tasks: Value = serde_json::from_slice(&default_list.stdout).expect("list JSON");
    let default_items = default_tasks
        .as_array()
        .or_else(|| default_tasks.get("tasks").and_then(Value::as_array))
        .expect("array");
    assert_eq!(
        default_items.len(),
        3,
        "all proposed tasks are listed by default: {default_tasks}"
    );

    // `--limit` bounds the response.
    let limited = workspace.run(
        &["task", "list", "--limit", "2", "--json"],
        None,
        "limited list",
    );
    let limited_tasks: Value = serde_json::from_slice(&limited.stdout).expect("list JSON");
    let limited_items = limited_tasks
        .as_array()
        .or_else(|| limited_tasks.get("tasks").and_then(Value::as_array))
        .expect("array");
    assert_eq!(limited_items.len(), 2);

    // A zero limit is a clear input error, not an empty success.
    let rejected = run_orbit(
        &workspace.work,
        &workspace.home,
        &["task", "list", "--limit", "0"],
        None,
    );
    assert!(
        !rejected.status.success(),
        "`--limit 0` must be rejected: {}",
        String::from_utf8_lossy(&rejected.stdout)
    );
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("limit"),
        "error must mention the limit: {}",
        String::from_utf8_lossy(&rejected.stderr)
    );
}

fn assert_task_titles(output: &Output, expected: &[&str]) {
    let value: Value = serde_json::from_slice(&output.stdout).expect("task array JSON");
    let tasks = value
        .as_array()
        .or_else(|| value.get("tasks").and_then(Value::as_array))
        .expect("task array");
    let mut titles = tasks
        .iter()
        .map(|task| task["title"].as_str().expect("task title").to_string())
        .collect::<Vec<_>>();
    titles.sort();

    let mut expected = expected
        .iter()
        .map(|title| (*title).to_string())
        .collect::<Vec<_>>();
    expected.sort();

    assert_eq!(titles, expected);
}

fn assert_orbit_search_titles(output: &Output, expected: &[&str]) {
    let response: Value = serde_json::from_slice(&output.stdout).expect("search response JSON");
    let mut titles = response["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|hit| hit["title"].as_str().expect("hit title").to_string())
        .collect::<Vec<_>>();
    titles.sort();

    let mut expected = expected
        .iter()
        .map(|title| (*title).to_string())
        .collect::<Vec<_>>();
    expected.sort();

    assert_eq!(titles, expected);
}

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
            &["workspace", "init", "--name", "task-tags-test"],
            None,
            "initialize workspace",
        );
        workspace
    }

    fn add_task(&self, title: &str, tags: &[&str]) -> Value {
        let mut args = vec![
            "task",
            "add",
            "--title",
            title,
            "--description",
            "Shared tag-search marker.",
            "--complexity",
            "low",
            "--json",
        ];
        for tag in tags {
            args.push("--tag");
            args.push(tag);
        }
        let output = self.run(&args, None, "add tagged task");
        serde_json::from_slice(&output.stdout).expect("task add JSON")
    }

    fn run(&self, args: &[&str], stdin: Option<&str>, label: &str) -> Output {
        let output = run_orbit(&self.work, &self.home, args, stdin);
        assert!(
            output.status.success(),
            "{label} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
}

fn run_orbit(cwd: &Path, home: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    let mut command = cargo_bin_cmd!("orbit");
    // ORB-11300: resetting HOME is not isolation. `ORBIT_REGISTRY_ROOT` and
    // `ORBIT_WORKSPACE` outrank it, so a suite launched from inside a managed
    // Orbit run used to add these fixture tasks to the live workspace.
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .args(args);
    if let Some(input) = stdin {
        command.write_stdin(input);
    }
    command.output().expect("run orbit")
}
