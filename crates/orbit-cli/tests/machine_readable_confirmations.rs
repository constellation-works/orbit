#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Confirmation-style commands (init, config, tool toggles, task update) must
//! honor `--format json` with one JSON document on stdout, and refuse inputs
//! that used to be persisted or reported as success without effect.
//!
//! Every command runs as a child process against a disposable `HOME` and
//! checkout, per `docs/DEVELOPMENT.md#safe-mutable-cli-fixtures`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::{TempDir, tempdir};

struct Fixture {
    _temp: TempDir,
    home: PathBuf,
    work: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        fs::create_dir_all(&home).expect("create home");
        fs::create_dir_all(&work).expect("create work");
        for args in [
            &["init", "--quiet"][..],
            &["config", "user.name", "Orbit Test"],
            &["config", "user.email", "orbit-test@example.com"],
            &["config", "commit.gpgsign", "false"],
            &["commit", "--quiet", "--allow-empty", "-m", "initial"],
        ] {
            let output = StdCommand::new("git")
                .arg("-C")
                .arg(&work)
                .args(args)
                .output()
                .expect("run git");
            assert!(output.status.success(), "git {args:?} failed");
        }
        Self {
            _temp: temp,
            home,
            work,
        }
    }

    fn orbit(&self) -> Command {
        fixture_orbit(&self.work, &self.home)
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = self
            .orbit()
            .args(args)
            .assert()
            .success()
            .get_output()
            .clone();
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "`orbit {}` must print one JSON document: {error}\nstdout:\n{}",
                args.join(" "),
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }

    fn init_machine_and_workspace(&self) {
        self.orbit()
            .args([
                "init",
                "--non-interactive",
                "--machine-name",
                "qa-host",
                "--task-prefix",
                "QA",
            ])
            .assert()
            .success();
        self.orbit()
            .args(["workspace", "init", "--name", "qa-workspace"])
            .assert()
            .success();
    }
}

fn fixture_orbit(work: &Path, home: &Path) -> Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(work)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("ORBIT_SKIP_HOST_PREREQUISITES", "1")
        .env_remove("ORBIT_FORMAT")
        .env_remove("NO_COLOR");
    command
}

#[test]
fn init_reports_the_machine_identity_as_json_and_flags_ignored_overrides() {
    let fixture = Fixture::new();

    let created = fixture.json(&[
        "init",
        "--non-interactive",
        "--machine-name",
        "qa-host",
        "--task-prefix",
        "QA",
        "--format",
        "json",
    ]);
    assert_eq!(created["machine"]["outcome"], "created");
    assert_eq!(created["machine"]["name"], "qa-host");
    assert_eq!(created["machine"]["task_prefix"], "QA");
    assert!(
        created["machine"]["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("hm_")),
        "machine id missing: {created}"
    );
    assert_eq!(created["warnings"], serde_json::json!([]));

    // The prefix is immutable: a differing flag on a re-run is not applied, and
    // saying nothing would let the operator believe it was.
    let repeated = fixture
        .orbit()
        .args([
            "init",
            "--non-interactive",
            "--task-prefix",
            "ZZ",
            "--format",
            "json",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("--task-prefix"))
        .get_output()
        .clone();
    let repeated: Value = serde_json::from_slice(&repeated.stdout).expect("re-init JSON");
    assert_eq!(repeated["machine"]["outcome"], "unchanged");
    assert_eq!(repeated["machine"]["task_prefix"], "QA");
}

#[test]
fn config_path_and_set_print_json_documents() {
    let fixture = Fixture::new();
    fixture.init_machine_and_workspace();

    let path = fixture.json(&["config", "path", "--global", "--format", "json"]);
    assert!(
        path["path"]
            .as_str()
            .is_some_and(|path| path.ends_with("config.toml")),
        "config path document: {path}"
    );

    let set = fixture.json(&[
        "config",
        "set",
        "--global",
        "automation.stall_window_minutes",
        "45",
        "--format",
        "json",
    ]);
    assert_eq!(set["key"], "automation.stall_window_minutes");
    assert_eq!(set["scope"], "global");
    let value = fixture.json(&[
        "config",
        "get",
        "automation.stall_window_minutes",
        "--format",
        "json",
    ]);
    assert_eq!(value["value"], 45);
}

#[test]
fn tool_toggles_and_mcp_registration_print_json_documents() {
    let fixture = Fixture::new();
    fixture.init_machine_and_workspace();

    let disabled = fixture.json(&["tool", "disable", "orbit.task.list", "--format", "json"]);
    assert_eq!(disabled["tool"], "orbit.task.list");
    assert_eq!(disabled["enabled"], false);
    let enabled = fixture.json(&["tool", "enable", "orbit.task.list", "--format", "json"]);
    assert_eq!(enabled["enabled"], true);

    let registered = fixture.json(&["mcp", "init", "--claude", "--format", "json"]);
    assert_eq!(registered["action"], "init");
    assert_eq!(registered["providers"], serde_json::json!(["claude"]));
    let removed = fixture.json(&["mcp", "remove", "--claude", "--format", "json"]);
    assert_eq!(removed["action"], "remove");
}

#[test]
fn task_update_refuses_empty_updates_and_unknown_pr_statuses() {
    let fixture = Fixture::new();
    fixture.init_machine_and_workspace();
    let id = String::from_utf8(
        fixture
            .orbit()
            .args(["task", "add", "--title", "qa", "--complexity", "low"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .expect("utf8 task id")
    .trim()
    .to_string();

    fixture
        .orbit()
        .args(["task", "update", &id])
        .assert()
        .failure()
        .stderr(predicate::str::contains("nothing to update"));

    // A typo used to be stored verbatim as the task's review status.
    fixture
        .orbit()
        .args(["task", "update", &id, "--pr-status", "aproved"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("approve"));
    let shown = fixture.json(&["task", "show", &id, "--format", "json"]);
    assert!(
        shown["pr_status"].is_null(),
        "a rejected value must not be persisted: {shown}"
    );

    fixture
        .orbit()
        .args(["task", "update", &id, "--pr-status", "approve"])
        .assert()
        .success();
    let shown = fixture.json(&["task", "show", &id, "--format", "json"]);
    assert_eq!(shown["pr_status"], "approve");
    fixture
        .orbit()
        .args(["task", "update", &id, "--pr-status", ""])
        .assert()
        .success();
    let shown = fixture.json(&["task", "show", &id, "--format", "json"]);
    assert!(shown["pr_status"].is_null(), "empty clears: {shown}");
}

#[test]
fn audit_prune_reports_a_bad_duration_before_asking_for_confirmation() {
    let fixture = Fixture::new();
    fixture.init_machine_and_workspace();
    fixture
        .orbit()
        .args(["audit", "prune", "--older-than", "garbage"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("duration"));
}

#[test]
fn routine_init_tool_scaffold_and_workspace_teardown_print_json_documents() {
    let fixture = Fixture::new();
    fixture.init_machine_and_workspace();

    let routine = fixture.json(&["routine", "init", "--format", "json"]);
    assert_eq!(routine["machine"]["name"], "qa-host");
    assert_eq!(routine["clock"]["installed"], false);

    let script = fixture.work.join("tools").join("hello");
    let scaffold = fixture.json(&[
        "tool",
        "scaffold",
        script.to_str().expect("utf8 path"),
        "--name",
        "hello",
        "--format",
        "json",
    ]);
    assert_eq!(scaffold["tool"], "hello");
    assert!(script.is_file(), "scaffold must still write the executable");
    assert!(
        scaffold["executable"]
            .as_str()
            .is_some_and(|path| path.ends_with("hello")),
        "scaffold document: {scaffold}"
    );

    // Teardown needs the operator capability; the override is per-child here.
    fixture
        .orbit()
        .env("ORBIT_OPERATOR", "1")
        .args(["workspace", "teardown", "qa-workspace"])
        .assert()
        .failure();
    let output = fixture
        .orbit()
        .env("ORBIT_OPERATOR", "1")
        .args([
            "workspace",
            "teardown",
            "qa-workspace",
            "--confirm",
            "--format",
            "json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let teardown: Value = serde_json::from_slice(&output.stdout).expect("teardown JSON");
    assert_eq!(teardown["workspace"], "qa-workspace");
    assert!(
        teardown["removed"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "teardown must list what it removed: {teardown}"
    );
    assert!(
        !fixture.work.join(".orbit").exists(),
        "teardown must still delete the data root"
    );
}

#[test]
fn root_pointing_at_a_file_is_refused_as_an_unusable_directory() {
    let fixture = Fixture::new();
    let file = fixture.work.join("not-a-dir");
    fs::write(&file, "x").expect("write file");

    fixture
        .orbit()
        .args(["--root", file.to_str().expect("utf8 path"), "task", "list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a usable Orbit root"))
        .stderr(predicate::str::contains("admission").not());
}

#[test]
fn task_dependencies_warn_when_unreadable_like_parent_but_are_still_recorded() {
    let fixture = Fixture::new();
    fixture.init_machine_and_workspace();
    let known = String::from_utf8(
        fixture
            .orbit()
            .args(["task", "add", "--title", "dep", "--complexity", "low"])
            .assert()
            .success()
            .stderr(predicate::str::contains("warning").not())
            .get_output()
            .stdout
            .clone(),
    )
    .expect("utf8 task id")
    .trim()
    .to_string();

    fixture
        .orbit()
        .args([
            "task",
            "add",
            "--title",
            "with known dependency",
            "--complexity",
            "low",
            "--dependencies",
            &known,
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("warning").not());

    let output = fixture
        .orbit()
        .args([
            "task",
            "add",
            "--title",
            "with typo dependency",
            "--complexity",
            "low",
            "--dependencies",
            "NOPE-1",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("NOPE-1"))
        .get_output()
        .clone();
    let id = String::from_utf8(output.stdout)
        .expect("utf8 task id")
        .trim()
        .to_string();
    let shown = fixture.json(&["task", "show", &id, "--format", "json"]);
    assert_eq!(shown["dependencies"], serde_json::json!(["NOPE-1"]));
}

#[test]
fn plain_search_rows_and_empty_plugin_doctor_have_no_dangling_separators() {
    let fixture = Fixture::new();
    fixture.init_machine_and_workspace();
    fixture
        .orbit()
        .args([
            "task",
            "add",
            "--title",
            "searchable widget",
            "--complexity",
            "low",
        ])
        .assert()
        .success();

    let output = fixture
        .orbit()
        .args(["search", "widget"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).expect("utf8 search output");
    assert!(
        stdout.contains("widget"),
        "the task should be found:\n{stdout}"
    );
    for line in stdout.lines() {
        assert!(
            !line.ends_with('\t'),
            "a plain row must not end with an empty field: {line:?}"
        );
    }

    let output = fixture
        .orbit()
        .args(["plugin", "doctor"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).expect("utf8 doctor output");
    assert!(
        !stdout.starts_with('\n'),
        "no rows means no separator before the summary: {stdout:?}"
    );
}
