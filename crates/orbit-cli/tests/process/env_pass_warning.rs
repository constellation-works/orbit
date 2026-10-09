//! A pass-listed variable the launching shell does not hold is reported at
//! drain/job start, on the run, and by `orbit doctor` [ORB-14777].
//!
//! The 2026-10-08 incident: a worker OAuth token named in `execution.env.pass`
//! was missing from the shell that started the drain, nothing said so, and
//! agents fell back to the desktop login. A warning is the contract: starting
//! still succeeds, the names (never values) are on stderr and the run record.

use std::fs;
use std::path::PathBuf;
use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::{TempDir, tempdir};
use toml_edit::{Array, DocumentMut, value};

use crate::{fixture_crew, git_repo};

const UNSET: &str = "FIXTURE_WORKER_TOKEN_UNSET";
const SET: &str = "FIXTURE_WORKER_TOKEN_SET";
const SECRET: &str = "secret-value-that-must-never-be-printed";
const JOB: &str = "env_pass_fixture";

struct Fixture {
    _temp: TempDir,
    home: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempdir().expect("fixture root");
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        fs::create_dir_all(&home).expect("home");
        git_repo::init(&repo);
        let fixture = Self {
            _temp: temp,
            home,
            repo,
        };
        fixture
            .command(&[])
            .args([
                "init",
                "--non-interactive",
                "--skip-host-prerequisites",
                "--machine-name",
                "env-pass",
                "--task-prefix",
                "EP",
            ])
            .assert()
            .success();
        fixture
            .command(&[])
            .args(["workspace", "init", "--name", "env-pass"])
            .assert()
            .success();
        let root = fixture.home.join(".orbit");
        fixture_crew::configure_sol(&root);
        fixture.pass_list(&["HOME", "PATH", UNSET, SET]);
        let jobs = root.join("resources/jobs");
        fs::create_dir_all(&jobs).expect("job catalog");
        for name in [JOB, "workspace_auto_pipeline"] {
            fs::write(
                jobs.join(format!("{name}.yaml")),
                format!(
                    "schemaVersion: 2\nkind: Job\nmetadata:\n  name: {name}\nspec:\n  state: enabled\n  kind: workflow\n  steps:\n    - id: nap\n      default_input:\n        seconds: 1\n      spec:\n        type: deterministic\n        action: sleep\n        config: {{}}\n"
                ),
            )
            .expect("fixture job");
        }
        fixture
    }

    fn pass_list(&self, names: &[&str]) {
        let path = self.home.join(".orbit/config.toml");
        let mut config: DocumentMut = fs::read_to_string(&path)
            .expect("read config")
            .parse()
            .expect("parse config");
        let mut pass = Array::new();
        for name in names {
            pass.push(*name);
        }
        config["execution"]["env"]["pass"] = value(pass);
        fs::write(&path, config.to_string()).expect("write config");
    }

    /// `set` names the pass-listed variables the launching shell holds.
    fn command(&self, set: &[(&str, &str)]) -> assert_cmd::Command {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env_remove(UNSET)
            .env_remove(SET);
        for (name, secret) in set {
            command.env(name, secret);
        }
        command
    }

    fn start(&self, set: &[(&str, &str)], args: &[&str]) -> (Value, String) {
        let output = self.command(set).args(args).output().expect("spawn orbit");
        assert!(output.status.success(), "{args:?} failed: {output:?}");
        let stderr = String::from_utf8(output.stderr.clone()).expect("stderr");
        assert!(
            !stderr.contains(SECRET) && !String::from_utf8_lossy(&output.stdout).contains(SECRET),
            "a pass-listed value reached the output of {args:?}"
        );
        (
            serde_json::from_slice(&output.stdout).expect("start JSON"),
            stderr,
        )
    }

    fn show(&self, run_id: &str) -> Output {
        self.command(&[(SET, SECRET)])
            .args(["run", "show", run_id, "--json", "--no-reconcile"])
            .output()
            .expect("run show")
    }

    fn doctor_row(&self, set: &[(&str, &str)]) -> Value {
        // `doctor` exits nonzero while unrelated rows (provider CLIs) fail in
        // a scratch home; its JSON still lists every row.
        let output = self
            .command(set)
            .args(["doctor", "--json"])
            .output()
            .expect("doctor");
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains(SECRET),
            "doctor printed a pass-listed value"
        );
        let rows: Value = serde_json::from_slice(&output.stdout).expect("doctor JSON");
        rows.as_array()
            .expect("doctor rows")
            .iter()
            .find(|row| row["check"] == "env-pass")
            .cloned()
            .unwrap_or_else(|| panic!("no env-pass row: {rows}"))
    }
}

fn warning_lines(stderr: &str) -> Vec<&str> {
    stderr
        .lines()
        .filter(|line| line.contains("execution.env.pass"))
        .collect()
}

fn assert_recorded(fixture: &Fixture, started: &Value, unset: &[&str]) {
    let run_id = started["run_id"].as_str().expect("run id");
    let shown = fixture.show(run_id);
    let text = String::from_utf8_lossy(&shown.stdout);
    assert!(
        !text.contains(SECRET),
        "run show printed a pass-listed value"
    );
    let shown: Value = serde_json::from_str(&text).expect("run show JSON");
    assert_eq!(shown["run"]["env_pass_unset"], serde_json::json!(unset));
    assert_eq!(
        shown["pipeline_state"]["env_pass_unset"]
            .as_array()
            .map_or(0, Vec::len),
        unset.len(),
        "the run's persisted state lists the unset names"
    );
}

#[test]
fn job_start_warns_once_naming_only_the_unset_variable_and_records_it() {
    let fixture = Fixture::new();
    let (started, stderr) = fixture.start(&[(SET, SECRET)], &["run", "job", JOB, "--json"]);
    let warnings = warning_lines(&stderr);
    assert_eq!(warnings.len(), 1, "one warning per start: {stderr}");
    assert!(warnings[0].contains(UNSET), "{stderr}");
    assert!(
        !warnings[0].contains(SET),
        "the set variable is not named as unset: {stderr}"
    );
    assert_recorded(&fixture, &started, &[UNSET]);

    let text = fixture
        .command(&[(SET, SECRET)])
        .args(["run", "show", started["run_id"].as_str().unwrap()])
        .output()
        .expect("run show text");
    assert!(
        String::from_utf8_lossy(&text.stdout).contains(&format!("Unset env: {UNSET}")),
        "run show names the unset variable: {text:?}"
    );
}

#[test]
fn drain_start_warns_and_records_the_unset_variable() {
    let fixture = Fixture::new();
    // Both CLI workflow entry points launch the worker from the installed
    // binary, so install the tested one the way the detached-worker tests do.
    let installed = fixture.home.join(".orbit/bin/orbit");
    fs::create_dir_all(installed.parent().unwrap()).expect("installation directory");
    fs::copy(env!("CARGO_BIN_EXE_orbit"), &installed).expect("install tested binary");

    let (started, stderr) = fixture.start(&[(SET, SECRET)], &["run", "auto", "--json"]);
    let warnings = warning_lines(&stderr);
    assert_eq!(warnings.len(), 1, "one warning per start: {stderr}");
    assert!(warnings[0].contains(UNSET), "{stderr}");
    assert_recorded(&fixture, &started, &[UNSET]);
}

#[test]
fn start_with_every_pass_listed_variable_set_is_silent_and_records_nothing() {
    let fixture = Fixture::new();
    let (started, stderr) = fixture.start(
        &[(SET, SECRET), (UNSET, SECRET)],
        &["run", "job", JOB, "--json"],
    );
    assert!(warning_lines(&stderr).is_empty(), "{stderr}");
    assert_recorded(&fixture, &started, &[]);
}

#[test]
fn doctor_warns_for_an_unset_pass_listed_variable_and_is_ok_when_all_are_set() {
    let fixture = Fixture::new();
    let unset = fixture.doctor_row(&[(SET, SECRET)]);
    assert_eq!(unset["status"], "warning", "{unset}");
    assert!(
        unset["message"].as_str().unwrap().contains(UNSET),
        "{unset}"
    );
    assert!(
        !unset["message"].as_str().unwrap().contains(SET),
        "the set variable is not reported: {unset}"
    );
    let hint = unset["remediation"].as_str().unwrap_or_default();
    assert!(
        hint.contains("login shell"),
        "doctor hints at the cause: {unset}"
    );

    let ok = fixture.doctor_row(&[(SET, SECRET), (UNSET, SECRET)]);
    assert_eq!(ok["status"], "ok", "{ok}");
}
