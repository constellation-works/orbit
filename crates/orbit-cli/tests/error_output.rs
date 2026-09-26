#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! How a failed invocation reports itself (`docs/design/terminal-interface/
//! specs/output-modes.md` §5): the error goes to stderr and stdout stays empty;
//! in `json`/`ndjson` mode the error is one JSON object; a command failure —
//! a missing record included — exits `1`, and a usage error exits `2`.

use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::{TempDir, tempdir};

const MISSING_TASK: &str = "ORB-99999999";
const MISSING_FRICTION: &str = "F2099-01-001";

struct Fixture {
    _temp: TempDir,
    home: PathBuf,
    work: PathBuf,
}

impl Fixture {
    /// An isolated HOME with no workspace: enough for argv that clap rejects.
    fn bare() -> Self {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let work = home.join("work");
        std::fs::create_dir_all(work.join(".git")).expect("create work repo");
        Self {
            _temp: temp,
            home,
            work,
        }
    }

    /// An isolated HOME with an initialized workspace, so a lookup reaches
    /// the store and misses there.
    fn workspace() -> Self {
        let fixture = Self::bare();
        let output = fixture.run(&["workspace", "init", "--name", "error-output-test"], &[]);
        assert!(
            output.status.success(),
            "workspace init failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        fixture
    }

    /// Two initialized workspaces sharing one HOME so re-homing can resolve
    /// the target workspace.
    fn two_workspaces() -> Self {
        let fixture = Self::workspace();
        let other_work = fixture.home.join("other_work");
        std::fs::create_dir_all(other_work.join(".git")).expect("create other work repo");
        let output = run_orbit(
            &other_work,
            &fixture.home,
            &["workspace", "init", "--name", "target-ws"],
            &[],
        );
        assert!(
            output.status.success(),
            "target workspace init failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        fixture
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        run_orbit(&self.work, &self.home, args, env)
    }
}

fn run_orbit(cwd: &Path, home: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("ORBIT_FORMAT")
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env_remove("COLUMNS");
    for (key, value) in env {
        command.env(key, value);
    }
    command.args(args).output().expect("run orbit")
}

fn describe(args: &[&str], output: &Output) -> String {
    format!(
        "`orbit {}` exited {:?}\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Assert the invocation failed with `exit_code`, printed nothing on stdout,
/// and reported one JSON error object on stderr; return that object.
fn json_failure(args: &[&str], output: &Output, exit_code: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(exit_code),
        "{}",
        describe(args, output)
    );
    assert!(
        output.stdout.is_empty(),
        "stdout must stay empty: {}",
        describe(args, output)
    );
    let error: Value = serde_json::from_slice(&output.stderr).unwrap_or_else(|err| {
        panic!(
            "stderr is one JSON document ({err}): {}",
            describe(args, output)
        )
    });
    assert!(
        error["error"]
            .as_str()
            .is_some_and(|message| !message.is_empty()),
        "the object carries a message: {}",
        describe(args, output)
    );
    assert!(
        error["code"].is_string(),
        "the object carries a code: {}",
        describe(args, output)
    );
    error
}

/// Assert the invocation failed with `exit_code`, printed nothing on stdout,
/// and reported a plain (non-JSON) message on stderr.
fn plain_failure(args: &[&str], output: &Output, exit_code: i32) {
    assert_eq!(
        output.status.code(),
        Some(exit_code),
        "{}",
        describe(args, output)
    );
    assert!(
        output.stdout.is_empty(),
        "stdout must stay empty: {}",
        describe(args, output)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("error: "),
        "a plain error is a labelled message: {}",
        describe(args, output)
    );
    assert!(
        serde_json::from_slice::<Value>(&output.stderr).is_err(),
        "a plain-mode error is not JSON: {}",
        describe(args, output)
    );
}

#[test]
fn a_missing_task_and_a_missing_friction_are_both_not_found_and_exit_1() {
    let fixture = Fixture::workspace();

    let task_args = ["task", "show", MISSING_TASK, "--json"];
    let task = json_failure(&task_args, &fixture.run(&task_args, &[]), 1);
    assert_eq!(task["code"], "task_not_found", "{task}");

    // Both spellings of JSON mode: the command's legacy flag and the global one.
    for friction_args in [
        ["friction", "show", MISSING_FRICTION, "--json"],
        ["friction", "show", MISSING_FRICTION, "--format=json"],
    ] {
        let friction = json_failure(&friction_args, &fixture.run(&friction_args, &[]), 1);
        assert_eq!(friction["code"], "friction_not_found", "{friction}");
    }
}

#[test]
fn a_command_failure_outside_json_mode_is_a_plain_message_on_stderr() {
    let fixture = Fixture::workspace();
    let args = ["friction", "show", MISSING_FRICTION];
    plain_failure(&args, &fixture.run(&args, &[]), 1);
}

#[test]
fn friction_mutations_on_a_missing_record_are_not_found_and_exit_1() {
    let fixture = Fixture::two_workspaces();

    for args in [
        vec![
            "friction",
            "update",
            MISSING_FRICTION,
            "--status",
            "triaged",
            "--json",
        ],
        vec![
            "friction",
            "update",
            MISSING_FRICTION,
            "--status",
            "triaged",
            "--format=json",
        ],
        vec!["friction", "resolve", MISSING_FRICTION, "--json"],
        vec!["friction", "resolve", MISSING_FRICTION, "--format=json"],
        vec![
            "friction",
            "rehome",
            MISSING_FRICTION,
            "--to-workspace",
            "target-ws",
            "--json",
        ],
        vec![
            "friction",
            "rehome",
            MISSING_FRICTION,
            "--to-workspace",
            "target-ws",
            "--format=json",
        ],
    ] {
        let error = json_failure(&args, &fixture.run(&args, &[]), 1);
        assert_eq!(error["code"], "friction_not_found", "{args:?}: {error}");
    }
}

#[test]
fn friction_mutations_with_malformed_id_or_invalid_field_return_invalid_input_and_exit_1() {
    let fixture = Fixture::two_workspaces();

    for args in [
        vec![
            "friction",
            "update",
            "malformed-id",
            "--status",
            "triaged",
            "--json",
        ],
        vec![
            "friction",
            "update",
            MISSING_FRICTION,
            "--status",
            "bogus-status",
            "--json",
        ],
        vec!["friction", "resolve", "malformed-id", "--json"],
        vec![
            "friction",
            "rehome",
            "malformed-id",
            "--to-workspace",
            "target-ws",
            "--json",
        ],
    ] {
        let error = json_failure(&args, &fixture.run(&args, &[]), 1);
        assert_eq!(error["code"], "invalid_input", "{args:?}: {error}");
    }
}

/// An argv clap rejects, and the environment it runs under.
type UsageCase<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)]);

#[test]
fn a_usage_error_in_json_mode_is_a_json_object_and_exits_2() {
    let fixture = Fixture::bare();
    let cases: &[UsageCase] = &[
        (&["--format", "json", "friction", "show", "--bogus"], &[]),
        (
            &["friction", "show", "F2026-01-001", "--bogus", "--json"],
            &[],
        ),
        (&["friction", "show"], &[("ORBIT_FORMAT", "json")]),
        (
            &["friction", "show", "--bogus"],
            &[("ORBIT_FORMAT", "ndjson")],
        ),
    ];
    for (args, env) in cases {
        let error = json_failure(args, &fixture.run(args, env), 2);
        assert_eq!(error["code"], "usage_error", "{args:?} {env:?}: {error}");
    }
}

#[test]
fn a_usage_error_outside_json_mode_stays_plain_and_exits_2() {
    let fixture = Fixture::bare();
    for args in [
        &["friction", "show", "--bogus"][..],
        &["--format", "table", "friction", "show", "--bogus"][..],
        &["task", "list", "--format", "xml"][..],
    ] {
        plain_failure(args, &fixture.run(args, &[]), 2);
    }
}

/// `--help` is not an error: JSON mode does not turn it into one.
#[test]
fn help_in_json_mode_is_still_help_on_stdout() {
    let fixture = Fixture::bare();
    let args = ["friction", "show", "--help", "--format", "json"];
    let output = fixture.run(&args, &[]);
    assert!(output.status.success(), "{}", describe(&args, &output));
    assert!(output.stderr.is_empty(), "{}", describe(&args, &output));
    assert!(
        serde_json::from_slice::<Value>(&output.stdout).is_err(),
        "help stays help: {}",
        describe(&args, &output)
    );
}
