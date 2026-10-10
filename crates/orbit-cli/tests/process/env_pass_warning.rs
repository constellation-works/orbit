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
const MACOS_TEMPLATE_VAR: &str = "__CF_USER_TEXT_ENCODING";
const SECRET: &str = "secret-value-that-must-never-be-printed";
#[cfg(unix)]
const AMBIENT_SECRET: &str = "ambient-secret-that-must-never-be-printed";
const NOT_PASSED: &str = "FIXTURE_NOT_PASS_LISTED";
const EMPTY: &str = "FIXTURE_EMPTY_TOKEN";
const JOB: &str = "env_pass_fixture";

struct Fixture {
    _temp: TempDir,
    home: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        Self::build(true)
    }

    #[cfg(target_os = "linux")]
    fn new_with_template_defaults() -> Self {
        Self::build(false)
    }

    fn build(override_pass_list: bool) -> Self {
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
        if override_pass_list {
            fixture.pass_list(&["HOME", "PATH", UNSET, SET]);
        }
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
            .env_remove(SET)
            .env_remove(NOT_PASSED)
            .env_remove(EMPTY);
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
            .env_remove(MACOS_TEMPLATE_VAR)
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

fn assert_warning_has_no_repeated_spaces(warning: &str) {
    assert!(
        !warning.as_bytes().windows(2).any(|spaces| spaces == b"  "),
        "unset env.pass warning contains consecutive spaces: {warning}"
    );
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
    assert_warning_has_no_repeated_spaces(warnings[0]);
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
    assert_warning_has_no_repeated_spaces(warnings[0]);
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

#[cfg(target_os = "linux")]
#[test]
fn doctor_does_not_warn_for_unset_template_defaults_after_fresh_init() {
    let fixture = Fixture::new_with_template_defaults();
    let row = fixture.doctor_row(&[]);
    assert_eq!(
        row["status"], "ok",
        "template defaults should not warn: {row}"
    );
    assert!(
        !row["message"]
            .as_str()
            .unwrap_or_default()
            .contains(MACOS_TEMPLATE_VAR),
        "the macOS-only template name is absent on Linux: {row}"
    );
}

#[cfg(unix)]
impl Fixture {
    fn clock_env(&self, contents: &str, mode: u32) {
        use std::os::unix::fs::PermissionsExt;

        let path = self.home.join(".orbit/clock.env");
        fs::write(&path, contents).expect("write clock.env");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("chmod clock.env");
    }

    fn tick_with(&self, set: &[(&str, &str)], dry_run: bool) -> (Value, String) {
        let mut command = self.command(set);
        command.args(["clock", "tick", "--json"]);
        if dry_run {
            command.arg("--dry-run");
        }
        let output = command.output().expect("clock tick");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            output.status.success(),
            "clock tick failed: {stdout}{stderr}"
        );
        assert!(
            [SECRET, AMBIENT_SECRET]
                .iter()
                .all(|secret| { !stdout.contains(secret) && !stderr.contains(secret) }),
            "clock tick printed a clock.env value: {stdout}{stderr}"
        );
        (serde_json::from_str(&stdout).expect("tick JSON"), stderr)
    }

    fn tick(&self) -> (Value, String) {
        self.tick_with(&[], false)
    }

    fn clock_probe(&self) {
        use std::os::unix::fs::PermissionsExt;

        // This fixed executor checks its actual environment after both detached
        // worker and provider dispatch. It emits only boolean results.
        let provider = self.home.join("codex");
        fs::write(&provider, format!(
            "#!/bin/sh\ncat >/dev/null\n\
             file=false; ambient=false; empty=false; filtered=false\n\
             [ \"${{{UNSET}-}}\" = '{SECRET}' ] && file=true\n\
             [ \"${{{SET}-}}\" = '{AMBIENT_SECRET}' ] && ambient=true\n\
             [ \"${{{EMPTY}-}}\" = '{SECRET}' ] && empty=true\n\
             [ \"${{{NOT_PASSED}+present}}\" != present ] && filtered=true\n\
             printf '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{\"file\":%s,\"ambient\":%s,\"empty\":%s,\"filtered\":%s}},\"error\":null}}\\n' \"$file\" \"$ambient\" \"$empty\" \"$filtered\"\n"
        )).expect("write credential probe");
        fs::set_permissions(&provider, fs::Permissions::from_mode(0o755)).unwrap();
        let root = self.home.join(".orbit");
        fs::write(
            root.join("resources/executors/codex.yaml"),
            serde_json::json!({
                "schemaVersion": 2, "kind": "Executor", "metadata": {"name": "codex"},
                "spec": {"executor_type": "direct_agent", "command": provider,
                    "args": [], "sandbox": "off", "env": {}}
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            root.join(format!("resources/jobs/{JOB}.yaml")),
            serde_json::json!({
                "schemaVersion": 2, "kind": "Job", "metadata": {"name": JOB},
                "spec": {"state": "enabled", "kind": "workflow", "steps": [{
                    "id": "probe", "spec": {"type": "agent_loop", "provider": "codex",
                        "backend": "cli", "description": "Probe the child environment",
                        "instruction": "Return the fixed fixture response",
                        "wall_clock_timeout_seconds": 10}
                }]}
            })
            .to_string(),
        )
        .unwrap();
        fs::write(self.repo.join(".orbit/routines/clock-env-fixture.yaml"), format!(
            "schemaVersion: 1\nname: clock-env-fixture\nenabled: true\ntrigger: {{ cron: '* * * * *' }}\ntarget: job:{JOB}\n"
        )).unwrap();
    }
}

/// The launchd/systemd clock holds no login environment, so `clock.env` is its
/// only source of operator credentials [ORB-15154]. Only pass-listed names are
/// loaded, and a file other users can read is refused outright.
#[cfg(unix)]
#[test]
fn clock_tick_loads_only_pass_listed_names_from_an_owner_only_clock_env() {
    let fixture = Fixture::new();
    fixture.clock_env(
        &format!("{UNSET}={SECRET}\nNOT_PASS_LISTED={SECRET}\n# {SET}=ignored\n"),
        0o600,
    );
    let (tick, stderr) = fixture.tick();
    assert_eq!(
        tick["clock_env_loaded"],
        serde_json::json!([UNSET]),
        "only the pass-listed name is loaded: {tick} {stderr}"
    );
    assert!(!stderr.contains("clock.env"), "{stderr}");

    fixture.clock_env(&format!("{UNSET}={SECRET}\n"), 0o644);
    let (tick, stderr) = fixture.tick();
    assert_eq!(tick["clock_env_loaded"], serde_json::json!([]), "{tick}");
    assert!(
        stderr.contains("clock.env") && stderr.contains("chmod 600"),
        "a group/world-readable file is refused with its fix: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn clock_credentials_reach_workers_with_ambient_precedence_and_dry_run_loads_nothing() {
    const CHILD: &str = "env_pass_warning::clock_worker_credentials_child";
    let temp = tempdir().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", CHILD, "--ignored", "--nocapture"]);
    let output = test_env::run_child_test(&mut command, CHILD, temp.path());
    test_env::assert_child_test_passed(CHILD, output.status, &output.stdout, &output.stderr);
}

#[cfg(unix)]
#[test]
#[ignore = "isolated clock worker credential boundary"]
fn clock_worker_credentials_child() {
    use std::time::{Duration, Instant};

    let fixture = Fixture::new();
    fixture.pass_list(&["HOME", "PATH", UNSET, SET, EMPTY]);
    fixture.clock_env(
        &format!("{UNSET}={SECRET}\n{SET}={SECRET}\n{EMPTY}={SECRET}\n{NOT_PASSED}={SECRET}\n"),
        0o600,
    );
    fixture.clock_probe();

    let ambient = [(SET, AMBIENT_SECRET), (EMPTY, "")];
    let (dry, _) = fixture.tick_with(&ambient, true);
    assert_eq!(dry["clock_env_loaded"], serde_json::json!([]));
    assert!(
        dry["reports"]
            .as_array()
            .unwrap()
            .iter()
            .all(|report| report["run_id"].is_null())
    );

    // A new routine first records its baseline. Seed a due cursor in this
    // isolated process so the real clock fires without waiting a minute.
    let connection = rusqlite::Connection::open(fixture.home.join(".orbit/orbit.db")).unwrap();
    let baseline = (chrono::Utc::now() - chrono::Duration::minutes(1)).to_rfc3339();
    connection
        .execute(
            "INSERT INTO routine_cursors (routine_name, baseline_at, last_slot, updated_at) \
         VALUES ('clock-env-fixture', ?1, NULL, ?1)",
            rusqlite::params![baseline],
        )
        .unwrap();
    drop(connection);

    let (tick, _) = fixture.tick_with(&ambient, false);
    assert_eq!(tick["clock_env_loaded"], serde_json::json!([UNSET, EMPTY]));
    let run = tick["reports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|report| report["routine"] == "clock-env-fixture")
        .unwrap_or_else(|| panic!("credential probe routine missing: {tick}"));
    let id = run["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("probe did not fire: {tick}"));
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let output = fixture.show(id);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            [SECRET, AMBIENT_SECRET]
                .iter()
                .all(|secret| { !stdout.contains(secret) && !stderr.contains(secret) }),
            "run observation printed a credential"
        );
        assert!(output.status.success(), "run show failed: {stdout}{stderr}");
        let shown: Value = serde_json::from_str(&stdout).unwrap();
        if shown["run"]["state"] == "success" {
            assert_eq!(shown["run"]["env_pass_unset"], serde_json::json!([]));
            for invariant in ["file", "ambient", "empty", "filtered"] {
                assert_eq!(
                    shown["pipeline_state"]["pipeline"]["probe"][invariant], true,
                    "worker/provider environment invariant {invariant}: {shown}"
                );
            }
            break;
        }
        assert!(
            !matches!(
                shown["run"]["state"].as_str(),
                Some("failed" | "cancelled" | "interrupted")
            ),
            "clock worker ended unsuccessfully: {shown}"
        );
        assert!(
            Instant::now() < deadline,
            "clock worker did not succeed: {shown}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(unix)]
#[test]
fn clock_credential_loader_preserves_the_process_environment_with_a_background_reader() {
    const CHILD: &str = "env_pass_warning::clock_credential_loader_child";
    let fixture = Fixture::new();
    fixture.clock_env(
        &format!("{UNSET}={SECRET}\n{SET}={SECRET}\n{EMPTY}={SECRET}\n"),
        0o600,
    );
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", CHILD, "--ignored", "--nocapture"])
        .env("HOME", &fixture.home)
        .env(SET, AMBIENT_SECRET)
        .env(EMPTY, "")
        .env_remove(UNSET);
    let output = test_env::run_child_test(&mut command, CHILD, fixture._temp.path());
    test_env::assert_child_test_passed(CHILD, output.status, &output.stdout, &output.stderr);
}

#[cfg(unix)]
#[test]
#[ignore = "isolated credential-loader boundary"]
fn clock_credential_loader_child() {
    let root = PathBuf::from(std::env::var_os("HOME").unwrap()).join(".orbit");
    std::thread::scope(|scope| {
        let (ready, started) = std::sync::mpsc::sync_channel(1);
        let (finish, finished) = std::sync::mpsc::sync_channel(1);
        let reader = scope.spawn(move || {
            ready.send(()).unwrap();
            finished.recv().unwrap();
            assert!(
                std::env::var_os(UNSET).is_none(),
                "loader must not export into the process"
            );
            assert_eq!(std::env::var(SET).unwrap(), AMBIENT_SECRET);
            assert_eq!(std::env::var(EMPTY).unwrap(), "");
        });
        started.recv().unwrap();
        let entries = orbit_common::security::operator_env::load_clock_env(
            &root,
            &[UNSET.to_string(), SET.to_string(), EMPTY.to_string()],
        )
        .unwrap();
        assert_eq!(
            entries,
            [
                (UNSET.to_string(), SECRET.to_string()),
                (EMPTY.to_string(), SECRET.to_string())
            ]
        );
        let policy =
            orbit_config::ResolvedConfig::load(&orbit_config::ConfigRoots::global_only(&root))
                .unwrap()
                .execution_env
                .with_defaults(&entries);
        let environment: std::collections::BTreeMap<_, _> =
            policy.agent_subprocess_env(&[EMPTY]).into_iter().collect();
        assert_eq!(environment.get(UNSET).map(String::as_str), Some(SECRET));
        assert_eq!(
            environment.get(SET).map(String::as_str),
            Some(AMBIENT_SECRET)
        );
        assert_eq!(
            environment.get(EMPTY).map(String::as_str),
            Some(""),
            "another workspace's default cannot be admitted through provider extras"
        );
        assert!(policy.unset_pass_names().is_empty());
        let diagnostic = format!("{policy:?}");
        assert!(
            !diagnostic.contains(SECRET) && !diagnostic.contains(AMBIENT_SECRET),
            "policy diagnostics must omit credential values"
        );
        finish.send(()).unwrap();
        reader.join().unwrap();
    });
}
