#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! End-to-end coverage for the borderless list contract (ADR-0307,
//! `docs/design/terminal-interface/specs/table-rendering.md`): a rendered list
//! is exactly one line per record, carries no box-drawing glyphs, and puts its
//! empty state on stderr.
//!
//! Both commands exercised here carry cell values far longer than a terminal
//! line — tool descriptions and task titles — which is what used to wrap a
//! single record across three or four lines.
//!
//! `assert_cmd` captures stdout through a pipe, so the default form these
//! subprocesses produce is the *plain* one: `auto` resolving against a
//! non-terminal sink, which suppresses the header (`specs/output-modes.md` §2).
//! Since [ORB-10586] each case asserts both that form and the header-bearing
//! `--format table`, which is the only way to reach the table rendering from a
//! pipe — and the one-line-per-record invariant is the same in both.

use std::fs;
use std::path::Path;
use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::{TempDir, tempdir};

const BOX_GLYPHS: &[char] = &['─', '│', '┌', '┐', '└', '┘', '├', '┤', '┬', '┴', '┼'];

const LONG_TITLE: &str = "Terminal tables are borderless with a one-line row invariant, and this title is deliberately far longer than any terminal is wide so that it has to be truncated rather than wrapped";

#[test]
fn tool_list_renders_one_line_per_tool() {
    let workspace = TestWorkspace::new();

    let json = workspace.run(&["tool", "list", "--all", "--json"], "tool list JSON");
    let tools: Value = serde_json::from_slice(&json.stdout).expect("tool list JSON");
    let expected = tools.as_array().expect("tool array").len();
    assert!(expected > 1, "fixture needs several tools to be meaningful");

    assert_both_forms(
        &workspace,
        &["tool", "list", "--all"],
        expected,
        "orbit tool list --all",
    );
}

#[test]
fn task_list_renders_one_line_per_task() {
    let workspace = TestWorkspace::new();
    for index in 0..3 {
        workspace.add_task(&format!("{LONG_TITLE} ({index})"));
    }

    assert_both_forms(&workspace, &["task", "list"], 3, "orbit task list");
}

#[test]
fn job_show_json_contains_no_human_step_rows() {
    let workspace = TestWorkspace::new();

    let json = workspace.run(
        &["job", "show", "epic_pipeline", "--format", "json"],
        "job show JSON",
    );
    let document: Value = serde_json::from_slice(&json.stdout).expect("job show JSON");
    assert!(document.is_object(), "job show returned {document}");
    assert!(
        !String::from_utf8_lossy(&json.stdout).contains("ID:"),
        "JSON output contained human step rows: {}",
        String::from_utf8_lossy(&json.stdout)
    );

    let human = workspace.run(&["job", "show", "epic_pipeline"], "job show human output");
    let human_stdout = String::from_utf8_lossy(&human.stdout);
    assert!(human_stdout.contains("Steps:"), "{human_stdout}");
    assert!(human_stdout.contains("ID:"), "{human_stdout}");
}

/// ORB-10571: the same property, for the other two commands covered by
/// `tests/output_goldens.rs`'s golden-file coverage. Both are seeded by
/// `orbit workspace init` itself (a default policy, a default skill
/// catalog), so no fixture setup is needed beyond initializing the
/// workspace.
#[test]
fn policy_list_renders_one_line_per_policy() {
    let workspace = TestWorkspace::new();

    let json = workspace.run(&["policy", "list", "--json"], "policy list JSON");
    let policies: Value = serde_json::from_slice(&json.stdout).expect("policy list JSON");
    let expected = policies.as_array().expect("policy array").len();
    assert!(expected >= 1, "a fresh workspace seeds a default policy");

    assert_both_forms(
        &workspace,
        &["policy", "list"],
        expected,
        "orbit policy list",
    );
}

#[test]
fn skill_list_renders_one_line_per_skill() {
    let workspace = TestWorkspace::new();

    let json = workspace.run(&["skill", "list", "--json"], "skill list JSON");
    let skills: Value = serde_json::from_slice(&json.stdout).expect("skill list JSON");
    let expected = skills.as_array().expect("skill array").len();
    // Each shipped skill's references load on demand, so the catalog is
    // deliberately small — the invariant under test is one line per record, not
    // the record count.
    assert!(
        expected >= 1,
        "a fresh workspace seeds the default skill catalog"
    );

    assert_both_forms(&workspace, &["skill", "list"], expected, "orbit skill list");
}

#[test]
fn a_list_with_no_matches_leaves_stdout_empty_and_explains_itself_on_stderr() {
    let workspace = TestWorkspace::new();
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
            &["workspace", "init", "--name", "table-rendering-test"],
            "initialize workspace",
        );
        workspace
    }

    fn add_task(&self, title: &str) {
        self.run(
            &[
                "task",
                "add",
                "--title",
                title,
                "--description",
                "A record whose rendered row must stay on one line.",
                "--complexity",
                "low",
                "--json",
            ],
            "add task",
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

fn run_orbit(work: &Path, home: &Path, args: &[&str]) -> Output {
    let mut command = cargo_bin_cmd!("orbit");
    // ORB-11300: shared with every other fixture that spawns `orbit`.
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(work)
        .env("HOME", home)
        .env("USERPROFILE", home)
        // Pin the geometry: width resolution is only consulted for a terminal
        // sink, and these assertions must not depend on the runner's terminal.
        .env("COLUMNS", "120")
        .args(args)
        .output()
        .expect("run orbit")
}
