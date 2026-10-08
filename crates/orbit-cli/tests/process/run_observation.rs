#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! `--no-reconcile` run reads leave stale runs and their task reservations
//! untouched [ORB-12941].
//!
//! The run-failure scan promises not to mutate run state, yet every run read
//! it relies on reconciles by default: an orphaned `pending`/`running` run is
//! finalized as `interrupted` and its reservations released, either lazily by
//! the query or at runtime open. Each fixture here spawns the real binary
//! against a disposable home so the runtime-open path is exercised too.
//!
//! An explicit run lookup also keeps a store failure distinct from a missing
//! run, so an unreadable record is not reported as nonexistent.
//!
//! Cancelling a live run is driven end to end as well: the launching CLI's
//! worker observer and the cancelling CLI race on the signalled worker's exit,
//! and the run must still end `cancelled`.
//!
//! A drain pass's surface reservation reaches readiness and `run show`.

use crate::{fixture_crew, git_repo};

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use orbit_core::OrbitRuntime;
use rusqlite::{Connection, params};
use serde_json::Value;

mod replay_crew;
mod run_show_display;

const STALE_RUNNING: &str = "jrun-20260920-0100";
const STALE_PENDING: &str = "jrun-20260920-0200";
const FAILED: &str = "jrun-20260920-0300";

/// A host resource sample well below every throttle mark.
struct CalmHost;

impl orbit_core::runtime::host_resource::HostResourceProbe for CalmHost {
    fn sample(
        &self,
        disk_paths: &[std::path::PathBuf],
    ) -> orbit_core::runtime::host_resource::HostResourceSample {
        orbit_core::runtime::host_resource::HostResourceSample {
            sampled_at: chrono::Utc::now(),
            cpu_percent: Some(10.0),
            memory_percent: Some(10.0),
            disks: disk_paths
                .iter()
                .map(|path| orbit_core::runtime::host_resource::DiskSample {
                    path: path.clone(),
                    used_percent: Some(10.0),
                })
                .collect(),
        }
    }
}

fn isolated_run_observation(test: &str) -> bool {
    const MARKER: &str = "ORBIT_TEST_SECURITY_SWEEP_CHILD";
    if std::env::var(MARKER).as_deref() == Ok(test) {
        return true;
    }
    let home = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(MARKER, test)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path());
    let logs = tempfile::tempdir().unwrap();
    let output = test_env::run_child_test(&mut command, test, logs.path());
    test_env::assert_child_test_passed(test, output.status, output.stdout, output.stderr);
    false
}

#[test]
fn successful_security_sweep_shows_filing_floor_source_and_excluded_alerts() {
    if !isolated_run_observation(
        "run_observation::successful_security_sweep_shows_filing_floor_source_and_excluded_alerts",
    ) {
        return;
    }
    let fixture = Fixture::init();
    let runtime =
        OrbitRuntime::from_roots(&fixture.home.join(".orbit"), &fixture.work.join(".orbit"))
            .unwrap();
    for source in [Some("workspace"), None] {
        let id = format!("jrun-cli-security-{}", source.unwrap_or("historical"));
        let now = chrono::Utc::now().to_rfc3339();
        fixture.db().execute(
            "INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,scheduled_at,started_at,finished_at,created_at) VALUES (?1,?2,'dependabot_alert_sweep_pipeline',1,'success',?3,?3,?3,?3)",
            params![id, fixture.workspace_id(), now],
        ).unwrap();
        let mut state = orbit_types::workflow::PipelineState::new(
            id.clone(),
            "dependabot_alert_sweep_pipeline".into(),
            serde_json::json!({}),
        );
        let mut output = serde_json::json!({
            "filed_count": 1, "min_severity": "high",
            "excluded_below_min_severity": [
                {"family": "dependabot", "number": 71, "severity": "moderate"},
                {"family": "code_scanning", "alert_number": 81, "security_severity": "moderate"}
            ]
        });
        if let Some(source) = source {
            output["min_severity_source"] = serde_json::json!(source);
        }
        state.record_pipeline_output("file", output);
        runtime.write_run_state(&id, &state).unwrap();
        let output = fixture
            .orbit()
            .args(["run", "show", &id, "--no-reconcile"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains("filed=1"), "{text}");
        assert!(
            text.contains(&format!(
                "min_severity=high ({})",
                source.unwrap_or("unavailable")
            )),
            "{text}"
        );
        assert!(
            text.contains("2 (dependabot #71, code_scanning #81)"),
            "successful sweeps must expose excluded alerts (2026-10-06/07 incident): {text}"
        );
    }
}

#[test]
fn shipped_security_job_uses_config_unless_run_input_overrides_it() {
    if !isolated_run_observation(
        "run_observation::shipped_security_job_uses_config_unless_run_input_overrides_it",
    ) {
        return;
    }
    let fixture = Fixture::init();
    // No gh is available: collection reports the capability gap, but filing
    // still resolves its floor through the shipped job's real templates.
    for (config, input, floor, source) in [
        ("", None, "moderate", "built-in"),
        (
            "[security_alert_sweep]\nmin_severity = \"high\"\n",
            None,
            "high",
            "workspace",
        ),
        (
            "[security_alert_sweep]\nmin_severity = \"high\"\n",
            Some("min_severity=critical"),
            "critical",
            "input",
        ),
        (
            "[security_alert_sweep]\nmin_severity = \"high\"\n",
            None,
            "high",
            "workspace",
        ),
    ] {
        fs::write(fixture.work.join(".orbit/config.toml"), config).unwrap();
        let mut command = fixture.orbit();
        command.args([
            "run",
            "job",
            "dependabot_alert_sweep_pipeline",
            "--wait",
            "--json",
        ]);
        if let Some(input) = input {
            command.args(["--input", input]);
        }
        command.timeout(std::time::Duration::from_secs(30));
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        let submitted: Value = serde_json::from_slice(&result.stdout).unwrap();
        let shown = fixture.json(&[
            "run",
            "show",
            submitted["run_id"].as_str().unwrap(),
            "--json",
        ]);
        let output = &shown["pipeline_state"]["pipeline"]["file"];
        assert_eq!(output["min_severity"], floor);
        assert_eq!(output["min_severity_source"], source);
        assert_eq!(
            fs::read_to_string(fixture.work.join(".orbit/config.toml")).unwrap(),
            config
        );
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    work: PathBuf,
}

impl Fixture {
    fn init() -> Self {
        let temp = tempfile::tempdir().expect("fixture tempdir");
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        fs::create_dir_all(&home).expect("fixture home");
        fs::create_dir_all(home.join("empty-bin")).expect("empty child PATH");
        git_repo::init(&work);
        let fixture = Self {
            _temp: temp,
            home,
            work,
        };
        fixture
            .orbit()
            .args(["workspace", "init", "--name", "fixture"])
            .assert()
            .success();
        // Deterministic jobs still freeze crew admission; pin fixture selection
        // while every child sees a PATH with no provider launcher.
        fixture_crew::configure_sol(&fixture.home.join(".orbit"));
        // Opening the workspace once creates the store the seed writes into.
        fixture
            .orbit()
            .args(["run", "history", "--no-reconcile", "--json"])
            .assert()
            .success();
        fixture.seed();
        fixture
    }

    fn orbit(&self) -> Command {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&self.work)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("PATH", self.home.join("empty-bin"));
        command
    }

    fn db(&self) -> Connection {
        Connection::open(self.home.join(".orbit").join("orbit.db")).expect("open fixture store")
    }

    fn workspace_id(&self) -> String {
        let config = fs::read_to_string(self.work.join(".orbit").join("config.yaml"))
            .expect("read workspace config");
        let config: serde_yaml::Value = serde_yaml::from_str(&config).expect("parse config");
        config["workspace_id"]
            .as_str()
            .expect("workspace id")
            .to_string()
    }

    /// A stale `running` run (impossible owner pid), a never-claimed
    /// `pending` run days past its grace window, each holding a reservation,
    /// and a finished `failed` run with a failed step as scan evidence.
    fn seed(&self) {
        let workspace_id = self.workspace_id();
        let orbit_dir = canonical(&self.work).join(".orbit");
        let orbit_dir = orbit_dir.to_string_lossy();
        let db = self.db();
        for (run_id, state, created_at, started_at, pid) in [
            (
                STALE_RUNNING,
                "running",
                "2026-09-20T01:00:00+00:00",
                Some("2026-09-20T01:00:01+00:00"),
                Some(999_999),
            ),
            (
                STALE_PENDING,
                "pending",
                "2026-09-20T02:00:00+00:00",
                None,
                None,
            ),
        ] {
            db.execute(
                "INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state,
                     scheduled_at, started_at, created_at, pid)
                 VALUES (?1, ?2, 'fixture_job', 1, ?3, ?4, ?5, ?4, ?6)",
                params![run_id, workspace_id, state, created_at, started_at, pid],
            )
            .expect("seed stale run");
            db.execute(
                "INSERT INTO task_reservations (reservation_id, workspace_orbit_dir,
                     workspace_id, task_ids_json, files_json, actor, created_at,
                     expires_at, owner_run_id)
                 VALUES (?1, ?2, ?3, '[]', ?4, 'fixture', ?5,
                     '2099-01-01T00:00:00+00:00', ?6)",
                params![
                    format!("reservation-{run_id}"),
                    orbit_dir,
                    workspace_id,
                    format!(r#"["src/{state}.rs"]"#),
                    created_at,
                    run_id,
                ],
            )
            .expect("seed reservation");
        }
        db.execute(
            "INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state,
                 scheduled_at, started_at, finished_at, duration_ms, created_at)
             VALUES (?1, ?2, 'fixture_job', 1, 'failed', '2026-09-20T03:00:00+00:00',
                 '2026-09-20T03:00:01+00:00', '2026-09-20T03:05:00+00:00', 299000,
                 '2026-09-20T03:00:00+00:00')",
            params![FAILED, workspace_id],
        )
        .expect("seed failed run");
        db.execute(
            "INSERT INTO job_run_steps (workspace_id, run_id, step_index, target_type,
                 target_id, state, started_at, finished_at, duration_ms, error_code,
                 error_message)
             VALUES (?1, ?2, 0, 'activity', 'implement', 'failed',
                 '2026-09-20T03:00:01+00:00', '2026-09-20T03:05:00+00:00', 299000,
                 'fixture_error', 'fixture step failed')",
            params![workspace_id, FAILED],
        )
        .expect("seed failed step");
    }

    /// Every run, step, and reservation row, for before/after comparison.
    fn snapshot(&self) -> Vec<String> {
        let db = self.db();
        let mut rows = Vec::new();
        for sql in [
            "SELECT run_id || '|' || state || '|' || IFNULL(finished_at, '-')
                 || '|' || IFNULL(duration_ms, '-') FROM job_runs ORDER BY run_id",
            "SELECT run_id || '|' || step_index || '|' || state FROM job_run_steps
                 ORDER BY run_id, step_index",
            "SELECT reservation_id || '|' || IFNULL(released_at, 'held') || '|'
                 || IFNULL(release_reason, '-') FROM task_reservations
                 ORDER BY reservation_id",
        ] {
            let mut statement = db.prepare(sql).expect("prepare snapshot");
            let values = statement
                .query_map([], |row| row.get::<_, String>(0))
                .expect("query snapshot")
                .collect::<Result<Vec<_>, _>>()
                .expect("collect snapshot");
            rows.extend(values);
        }
        rows
    }

    fn run_state(&self, run_id: &str) -> String {
        self.db()
            .query_row(
                "SELECT state FROM job_runs WHERE run_id = ?1",
                [run_id],
                |row| row.get(0),
            )
            .expect("read run state")
    }

    fn insert_pending_run(&self, run_id: &str) {
        let workspace_id = self.workspace_id();
        let now = chrono::Utc::now().to_rfc3339();
        self.db()
            .execute(
                "INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state,
                     scheduled_at, created_at)
                 VALUES (?1, ?2, 'task_pr_pipeline', 1, 'pending', ?3, ?3)",
                params![run_id, workspace_id, now],
            )
            .expect("insert pending leaf run");
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = self.orbit().args(args).output().expect("spawn orbit");
        assert!(
            output.status.success(),
            "{args:?} failed with {}\nstdout: {}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if std::env::var_os("ORBIT_QA_TRACE_CLI").is_some() {
            writeln!(
                std::io::stderr(),
                "QA_CLI {}",
                serde_json::json!({
                    "test": std::thread::current().name(), "argv": args,
                    "exit_code": output.status.code(),
                })
            )
            .expect("write opt-in CLI evidence");
        }
        serde_json::from_slice(&output.stdout).expect("json output")
    }

    /// The structured error a failing `--json` invocation writes to stderr.
    fn failure(&self, args: &[&str]) -> Value {
        let output = self.orbit().args(args).output().expect("spawn orbit");
        assert!(!output.status.success(), "{args:?} unexpectedly succeeded");
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Legacy `orbit logs` prints a deprecation line ahead of the error.
        let start = stderr.find('{').unwrap_or(0);
        serde_json::from_str(&stderr[start..])
            .unwrap_or_else(|_| panic!("{args:?} stderr is not one JSON error: {stderr}"))
    }
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Every read form the run-failure scan permits, including the latest-run
/// defaults, leaves the stale runs and their reservations exactly as stored
/// while still returning the finished-run and step-failure evidence.
#[test]
fn readiness_bootstrap_preserves_stale_runs_and_held_reservations() {
    let fixture = Fixture::init();
    let before = fixture.snapshot();
    let report = fixture.json(&[
        "run",
        "readiness",
        "--concurrency",
        "3",
        "--limit",
        "1",
        "--json",
    ]);
    assert_eq!(report["capacity"]["max_active_leaf_runs"], 3);
    assert!(report["tasks"].as_array().unwrap().len() <= 1);
    assert_eq!(
        fixture.snapshot(),
        before,
        "readiness must not reconcile or reserve at runtime open"
    );
    fixture
        .orbit()
        .args(["run", "readiness", "--limit", "0", "--json"])
        .assert()
        .failure();
    assert_eq!(
        fixture.snapshot(),
        before,
        "invalid readiness must also preserve state"
    );
}

#[test]
fn job_replay_cli_reexecutes_deterministic_fixture_with_persisted_lineage() {
    let fixture = Fixture::init();
    let jobs = fixture.home.join(".orbit/resources/jobs");
    fs::create_dir_all(&jobs).unwrap();
    fs::write(jobs.join("fixture_job.yaml"), "schemaVersion: 2\nkind: Job\nmetadata:\n  name: fixture_job\nspec:\n  state: enabled\n  kind: workflow\n  steps:\n    - id: nap\n      spec:\n        type: deterministic\n        action: sleep\n        config: {}\n").unwrap();
    let input = serde_json::json!({"seconds": 0, "marker": "persisted-replay-input"});
    fixture
        .db()
        .execute(
            "UPDATE job_runs SET input_json=?1 WHERE run_id=?2",
            params![input.to_string(), FAILED],
        )
        .unwrap();
    let source = fixture.json(&["run", "show", FAILED, "--no-reconcile", "--json"]);
    let replay = fixture.json(&["job", "replay", FAILED, "--json"]);
    assert_eq!(replay["success"], true);
    assert_eq!(replay["source_run_id"], FAILED);
    let id = replay["run_id"].as_str().unwrap();
    assert_ne!(id, FAILED);
    let stored = fixture.json(&["run", "show", id, "--no-reconcile", "--json"]);
    assert_eq!(stored["run"]["state"], "success");
    assert_eq!(stored["run"]["retry_source_run_id"], FAILED);
    let persisted_input: String = fixture
        .db()
        .query_row(
            "SELECT input_json FROM job_runs WHERE run_id=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&persisted_input).unwrap(),
        input
    );
    assert_eq!(
        fixture.json(&["run", "show", FAILED, "--no-reconcile", "--json"]),
        source
    );
    fixture
        .orbit()
        .args(["job", "replay", "jrun-missing-fixture", "--json"])
        .assert()
        .failure();
    assert_eq!(
        fixture.json(&["run", "show", FAILED, "--no-reconcile", "--json"]),
        source
    );
}

#[test]
fn no_reconcile_run_reads_leave_stale_runs_and_reservations_unchanged() {
    let fixture = Fixture::init();
    let before = fixture.snapshot();
    assert!(
        before.iter().any(|row| row.ends_with("|held|-")),
        "fixture must start with held reservations: {before:?}"
    );

    let history = fixture.json(&[
        "run",
        "history",
        "--limit",
        "50",
        "--no-reconcile",
        "--json",
    ]);
    let states = history["runs"]
        .as_array()
        .expect("runs")
        .iter()
        .map(|run| {
            (
                run["run_id"].as_str().unwrap().to_string(),
                run["state"].as_str().unwrap().to_string(),
            )
        })
        .collect::<Vec<_>>();
    for (run_id, state) in [
        (STALE_RUNNING, "running"),
        (STALE_PENDING, "pending"),
        (FAILED, "failed"),
    ] {
        assert!(
            states.contains(&(run_id.to_string(), state.to_string())),
            "history must list {run_id} as {state}: {states:?}"
        );
    }

    let shown = fixture.json(&["run", "show", FAILED, "--no-reconcile", "--json"]);
    assert_eq!(shown["run"]["state"], "failed");
    assert!(
        shown["steps"]
            .as_array()
            .expect("steps")
            .iter()
            .any(|step| { step["state"] == "failed" && step["error_code"] == "fixture_error" }),
        "show must return the step failure: {shown}"
    );
    for run_id in [STALE_RUNNING, STALE_PENDING] {
        let shown = fixture.json(&["run", "show", run_id, "--no-reconcile", "--json"]);
        assert!(
            matches!(shown["run"]["state"].as_str(), Some("running" | "pending")),
            "show must report {run_id} as stored: {shown}"
        );
    }

    for args in [
        vec!["run", "logs", FAILED, "--no-reconcile", "--json"],
        vec![
            "run",
            "logs",
            FAILED,
            "--step",
            "implement",
            "--no-reconcile",
            "--json",
        ],
        vec!["run", "logs", STALE_RUNNING, "--no-reconcile", "--json"],
        vec!["run", "events", FAILED, "--no-reconcile", "--json"],
        vec!["run", "events", STALE_PENDING, "--no-reconcile", "--json"],
        vec!["run", "show", "--no-reconcile", "--json"],
        vec!["run", "logs", "--no-reconcile", "--json"],
        vec!["run", "events", "--no-reconcile", "--json"],
    ] {
        fixture.json(&args);
        assert_eq!(fixture.snapshot(), before, "{args:?} mutated run state");
    }
    assert_eq!(fixture.snapshot(), before);
}

/// Without the flag the same reads keep reconciling, including `run events`
/// for an unrelated run, which reconciles every stale run at runtime open.
#[test]
fn default_run_reads_still_reconcile_stale_runs() {
    let fixture = Fixture::init();
    fixture
        .orbit()
        .args(["run", "events", FAILED, "--json"])
        .assert()
        .success();
    assert_eq!(fixture.run_state(STALE_RUNNING), "interrupted");
    assert_eq!(fixture.run_state(STALE_PENDING), "interrupted");
    assert_eq!(fixture.run_state(FAILED), "failed");
    let after = fixture.snapshot();
    assert!(
        after
            .iter()
            .filter(|row| row.starts_with("reservation-"))
            .all(|row| row.ends_with("|stale_run_reconciled")),
        "reconciliation must release the stale runs' reservations: {after:?}"
    );

    let fixture = Fixture::init();
    let history = fixture.json(&["run", "history", "--json"]);
    assert!(
        history["runs"]
            .as_array()
            .expect("runs")
            .iter()
            .filter(|run| run["run_id"] != FAILED)
            .all(|run| run["state"] == "interrupted"),
        "history must reconcile by default: {history}"
    );
}

/// An explicit run lookup reports why the stored run is unreadable instead
/// of claiming it does not exist; only a run with no row is `not_found`.
/// Legacy `orbit logs` follows the same rule.
#[test]
fn explicit_run_lookup_preserves_store_errors() {
    const MISSING: &str = "jrun-20260920-9999";
    let fixture = Fixture::init();
    fixture
        .db()
        .execute(
            "UPDATE job_runs SET state = 'corrupt_state' WHERE run_id = ?1",
            [FAILED],
        )
        .expect("corrupt stored run");

    let lookups = |run_id: &'static str| {
        [
            vec!["run", "show", run_id, "--no-reconcile", "--json"],
            vec!["run", "show", run_id, "--json"],
            vec!["run", "logs", run_id, "--no-reconcile", "--json"],
            vec!["run", "events", run_id, "--no-reconcile", "--json"],
            vec!["run", "trace", run_id, "--json"],
            vec!["logs", run_id, "--json"],
        ]
    };
    for args in lookups(FAILED) {
        let error = fixture.failure(&args);
        assert_eq!(error["code"], "store_error", "{args:?} -> {error}");
        assert!(
            error["error"]
                .as_str()
                .is_some_and(|message| message.contains("corrupt_state")),
            "{args:?} must keep the store diagnostic: {error}"
        );
    }
    for args in lookups(MISSING) {
        let error = fixture.failure(&args);
        assert_eq!(error["code"], "job_run_not_found", "{args:?} -> {error}");
        assert!(
            error["error"]
                .as_str()
                .is_some_and(|message| message.contains(MISSING)),
            "{args:?} must name the missing run: {error}"
        );
    }
}

/// The public `job run` alias executes locally, and its completed audit tree
/// remains readable after the worker exits. Cancelling this terminal fixture
/// cannot signal a process or replace its successful outcome.
#[test]
fn job_run_alias_produces_a_completed_trace_and_terminal_cancel_is_stable() {
    let fixture = Fixture::init();
    let jobs = fixture.home.join(".orbit/resources/jobs");
    fs::create_dir_all(&jobs).unwrap();
    fs::write(jobs.join("alias_fixture.yaml"), "schemaVersion: 2\nkind: Job\nmetadata:\n  name: alias_fixture\nspec:\n  state: enabled\n  kind: workflow\n  steps:\n    - id: nap\n      default_input:\n        seconds: 0\n      spec:\n        type: deterministic\n        action: sleep\n        config: {}\n").unwrap();
    let completed = fixture.json(&["job", "run", "alias_fixture", "--wait", "--json"]);
    assert_eq!(completed["state"], "success");
    let run_id = completed["run_id"].as_str().unwrap();
    let shown = fixture.json(&["run", "show", run_id, "--no-reconcile", "--json"]);
    assert_eq!(shown["run"]["state"], "success");
    let trace = fixture.json(&["run", "trace", run_id, "--json"]);
    assert_eq!(trace["run_id"], run_id);
    assert_eq!(trace["job_id"], "alias_fixture");
    let roots = trace["roots"].as_array().unwrap();
    assert!(
        !roots.is_empty(),
        "completed run has an audit tree: {trace}"
    );
    assert!(
        roots
            .iter()
            .all(|node| node["event"].is_object() && node["children"].is_array())
    );
    fn contains_step(node: &Value, step: &str) -> bool {
        node["event"]["step_id"] == step
            || node["children"]
                .as_array()
                .unwrap()
                .iter()
                .any(|child| contains_step(child, step))
    }
    assert!(
        roots.iter().any(|node| contains_step(node, "nap")),
        "trace retains the executed step: {trace}"
    );
    let cancelled = fixture.json(&["run", "cancel", run_id, "--confirm", "--json"]);
    assert_eq!(cancelled["outcome"], "already_terminal");
    assert_eq!(cancelled["previous_state"], "success");
    assert_eq!(cancelled["final_state"], "success");
    assert_eq!(cancelled["signal_attempted"], false);
    assert_eq!(cancelled["provider_processes_stopped"], 0);
    assert_eq!(fixture.run_state(run_id), "success");
}

/// The CLI's default task-leaf cancel requeues with its reason and leaves the
/// candidate in place; `--block` retains the previous blocked transition.
#[test]
fn cancel_task_leaf_requeues_by_default_and_block_is_explicit() {
    let fixture = Fixture::init();
    let candidate = fixture.work.join("src").join("candidate.rs");
    fs::create_dir_all(candidate.parent().unwrap()).unwrap();
    fs::write(&candidate, "candidate content\n").unwrap();

    for (run_id, block, expected_status) in [
        ("jrun-20261005-0434-1", false, "backlog"),
        ("jrun-20261005-0434-2", true, "blocked"),
    ] {
        fixture.insert_pending_run(run_id);
        let created = fixture.json(&[
            "task",
            "add",
            "--title",
            "Cancel candidate",
            "--description",
            "Keep the candidate when cancelling the leaf",
            "--plan",
            "Resume the candidate after cancellation",
            "--complexity",
            "low",
            "--context",
            "file:src/candidate.rs",
            "--json",
        ]);
        let task_id = created["id"].as_str().expect("task id").to_string();
        fixture
            .orbit()
            .args([
                "task",
                "update",
                &task_id,
                "--status",
                "in-progress",
                "--job-run-id",
                run_id,
                "--json",
            ])
            .assert()
            .success();

        let mut args = vec!["run", "cancel", run_id, "--confirm", "--json"];
        if block {
            args.push("--block");
        }
        args.extend(["--reason", "preserve this candidate for later"]);
        let result = fixture.json(&args);
        assert_eq!(result["outcome"], "cancelled");

        let task = fixture.json(&["task", "show", &task_id, "--json"]);
        assert_eq!(task["status"], expected_status);
        assert_eq!(task["plan"], "Resume the candidate after cancellation");
        assert_eq!(task["context_files"][0], "file:src/candidate.rs");
        assert_eq!(
            fs::read_to_string(&candidate).unwrap(),
            "candidate content\n"
        );

        let orbit_root = fixture.work.join(".orbit");
        let runtime = OrbitRuntime::from_roots(&fixture.home.join(".orbit"), &orbit_root)
            .expect("open fixture runtime");
        let history = runtime.get_task_history(&task_id).expect("task history");
        let entry = history.last().expect("cancellation status event");
        assert!(
            entry
                .note
                .as_deref()
                .is_some_and(|note| { note.contains("preserve this candidate for later") })
        );
        assert_eq!(
            entry.event,
            if block {
                "workflow_run_failed"
            } else {
                "workflow_run_cancelled"
            }
        );
    }
}

/// Capture the pre-change stable timestamp independently of Orbit's probe.
///
/// `None` when the sandbox refuses to run `ps`: there is no pre-change token
/// to capture, so the caller returns after this reports the skip on stderr.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(clippy::print_stderr)]
fn ps_lstart_utc(pid: u32) -> Option<String> {
    let output = match test_env::ps_lstart_utc(pid) {
        test_env::PsRun::Ran(output) => output,
        test_env::PsRun::Denied(reason) => {
            eprintln!("SKIP: {reason}");
            return None;
        }
    };
    assert!(output.status.success(), "ps fixture: {output:?}");
    let raw = String::from_utf8(output.stdout).unwrap().trim().to_string();
    assert!(!raw.is_empty(), "ps must describe the live fixture process");
    Some(raw)
}

/// Re-execution clears PATH only in the child, without changing the parallel
/// test runner's environment. The fixture token comes from the old ps probe.
// Linux: the production /proc start-time probe must match persisted ps tokens with PATH empty.
#[cfg(target_os = "linux")]
#[test]
fn linux_start_identity_without_ps_matches_pre_change_owner_tokens() {
    use orbit_common::process::identity::{
        ProbeOutcome, STABLE_TOKEN_PREFIX, STABLE_TOKEN_PREFIX_V1, current_pid_namespace,
        legacy_lstart_matches, probe_process_start_identity, stable_tokens_match,
    };

    const PID: &str = "ORBIT_TEST_LSTART_PID";
    const RAW: &str = "ORBIT_TEST_LSTART_RAW";
    const TOKEN: &str = "ORBIT_TEST_LSTART_TOKEN";
    if let Ok(pid) = std::env::var(PID) {
        assert_eq!(std::env::var("PATH").unwrap(), "");
        let pid = pid.parse().unwrap();
        let raw = std::env::var(RAW).unwrap();
        let token = std::env::var(TOKEN).unwrap();
        assert_eq!(
            probe_process_start_identity(pid),
            ProbeOutcome::Token(token.clone()),
            "PATH-free kernel identity must equal the pre-change ps fixture"
        );
        assert!(stable_tokens_match(
            &token,
            &format!("{STABLE_TOKEN_PREFIX_V1}{raw}")
        ));
        assert!(legacy_lstart_matches(pid, &raw));
        assert!(!legacy_lstart_matches(pid, "different start time"));
        assert_eq!(
            probe_process_start_identity(u32::MAX),
            ProbeOutcome::NoProcess
        );
        return;
    }

    let pid = std::process::id();
    let Some(raw) = ps_lstart_utc(pid) else {
        return;
    };
    let namespace = current_pid_namespace().expect("Linux PID namespace");
    let token = format!("{STABLE_TOKEN_PREFIX}pidns={namespace}:{raw}");
    assert_eq!(
        probe_process_start_identity(pid),
        ProbeOutcome::Token(token.clone())
    );
    let mut child = Command::new(std::env::current_exe().unwrap());
    test_env::clear_inherited_authority(|name| {
        child.env_remove(name);
    });
    let output = child
        .args([
            "--exact",
            "run_observation::linux_start_identity_without_ps_matches_pre_change_owner_tokens",
            "--nocapture",
        ])
        .env("PATH", "")
        .env("TZ", "Pacific/Honolulu")
        .env("LC_ALL", "C.UTF-8")
        .env(PID, pid.to_string())
        .env(RAW, raw)
        .env(TOKEN, token)
        .timeout(std::time::Duration::from_secs(10))
        .output()
        .unwrap();
    assert!(output.status.success(), "PATH-free probe: {output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"),
        "the child must execute the identity assertions: {output:?}"
    );
}

/// Persist real ps-derived v2, v1 and legacy tokens in the store, then drive
/// cancellation through the built binary with no ps on its PATH.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn cancelling_owners_with_pre_change_ps_tokens_still_signals_them() {
    use orbit_common::process::identity::{
        STABLE_TOKEN_PREFIX, STABLE_TOKEN_PREFIX_V1, current_pid_namespace, process_is_alive,
    };

    let fixture = Fixture::init();
    for format in ["v2", "v1", "legacy"] {
        let owner = test_env::spawn_unrelated_process();
        let Some(raw) = ps_lstart_utc(owner.pid()) else {
            return;
        };
        let token = match format {
            "v2" => format!(
                "{STABLE_TOKEN_PREFIX}pidns={}:{raw}",
                current_pid_namespace().unwrap_or("-")
            ),
            "v1" => format!("{STABLE_TOKEN_PREFIX_V1}{raw}"),
            _ => raw,
        };
        let run_id = format!("jrun-pre-change-{format}");
        let now = chrono::Utc::now().to_rfc3339();
        fixture
            .db()
            .execute(
                "INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state,
                scheduled_at, started_at, created_at, pid, pid_start_time)
             VALUES (?1, ?2, 'fixture_job', 1, 'running', ?3, ?3, ?3, ?4, ?5)",
                params![run_id, fixture.workspace_id(), now, owner.pid(), token],
            )
            .unwrap();
        let cancelled = fixture.json(&["run", "cancel", &run_id, "--confirm", "--json"]);
        assert_eq!(cancelled["outcome"], "cancelled", "{format}: {cancelled}");
        assert!(
            cancelled["signal_outcome"]
                .as_str()
                .is_some_and(|outcome| outcome.starts_with("terminated_")),
            "pre-change {format} owner token must still verify: {cancelled}"
        );
        assert!(
            !process_is_alive(owner.pid()),
            "{format} owner must be stopped"
        );
    }
}

/// Cancelling a live run ends its worker by signal while the launching CLI is
/// still observing that worker. The observer must treat the signalled exit as
/// the cancellation's own outcome rather than a worker failure racing it to
/// the terminal state, and the cancelled outcome must survive a repeat
/// cancel.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn cancelling_a_live_run_keeps_the_signalled_worker_exit_as_its_outcome() {
    use std::process::{Child, Stdio};
    use std::time::{Duration, Instant};

    /// Kills and reaps the launcher on every exit path, panics included.
    struct ReapOnDrop(Child);
    impl Drop for ReapOnDrop {
        fn drop(&mut self) {
            if matches!(self.0.try_wait(), Ok(None)) {
                let _ = self.0.kill();
            }
            let _ = self.0.wait();
        }
    }
    /// The worker runs in its own session, outside the launcher's reach.
    /// Kill its process group if the test fails before the cancellation is
    /// shown to have stopped it.
    struct KillWorkerGroup(Option<libc::pid_t>);
    impl Drop for KillWorkerGroup {
        fn drop(&mut self) {
            if let Some(pid) = self.0 {
                unsafe { libc::kill(-pid, libc::SIGKILL) };
            }
        }
    }

    let fixture = Fixture::init();
    // Native Linux/macOS owner verification must work with no ps on PATH.
    let jobs = fixture.home.join(".orbit/resources/jobs");
    fs::create_dir_all(&jobs).unwrap();
    fs::write(jobs.join("cancel_fixture.yaml"), "schemaVersion: 2\nkind: Job\nmetadata:\n  name: cancel_fixture\nspec:\n  state: enabled\n  kind: workflow\n  steps:\n    - id: nap\n      default_input:\n        seconds: 60\n      spec:\n        type: deterministic\n        action: sleep\n        config: {}\n").unwrap();

    // `--wait` keeps the launching CLI alive as the worker's observer.
    let launcher_log = fixture.work.join("launcher.log");
    let log = fs::File::create(&launcher_log).unwrap();
    let mut launcher = std::process::Command::new(env!("CARGO_BIN_EXE_orbit"));
    test_env::clear_inherited_authority(|name| {
        launcher.env_remove(name);
    });
    launcher
        .args(["job", "run", "cancel_fixture", "--wait", "--json"])
        .current_dir(&fixture.work)
        .env("HOME", &fixture.home)
        .env("USERPROFILE", &fixture.home)
        .env("PATH", fixture.home.join("empty-bin"))
        .stdin(Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log);
    let mut launcher = ReapOnDrop(launcher.spawn().unwrap());
    let launcher_output = || fs::read_to_string(&launcher_log).unwrap_or_default();

    let deadline = Instant::now() + Duration::from_secs(30);
    let (run_id, worker_pid) = loop {
        let owned: Option<(String, Option<i64>)> = fixture
            .db()
            .query_row(
                "SELECT run_id, pid FROM job_runs WHERE job_id = 'cancel_fixture'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok();
        if let Some((run_id, Some(pid))) = owned
            && fixture.run_state(&run_id) == "running"
        {
            break (run_id, pid);
        }
        assert!(
            Instant::now() < deadline,
            "worker never started: {}",
            launcher_output()
        );
        assert!(
            launcher.0.try_wait().unwrap().is_none(),
            "launcher exited before cancellation: {}",
            launcher_output()
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_ne!(worker_pid, i64::from(std::process::id()));
    assert_ne!(
        worker_pid,
        i64::from(launcher.0.id()),
        "a worker owns the run"
    );
    let worker_pid = libc::pid_t::try_from(worker_pid).unwrap();
    let mut worker = KillWorkerGroup(Some(worker_pid));

    let cancelled = fixture.json(&["run", "cancel", &run_id, "--confirm", "--json"]);
    assert_eq!(cancelled["outcome"], "cancelled", "{cancelled}");
    assert_eq!(cancelled["previous_state"], "running");
    assert_eq!(cancelled["final_state"], "cancelled");
    assert_eq!(cancelled["signal_attempted"], true, "{cancelled}");
    assert!(
        cancelled["signal_outcome"]
            .as_str()
            .is_some_and(|outcome| outcome.starts_with("terminated_")),
        "cancellation must acknowledge terminating the worker: {cancelled}"
    );

    let deadline = Instant::now() + Duration::from_secs(30);
    while launcher.0.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "launcher outlived its cancelled run: {}",
            launcher_output()
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    assert_eq!(
        unsafe { libc::kill(worker_pid, 0) },
        -1,
        "the cancelled run's worker is gone"
    );
    worker.0 = None;

    let shown = fixture.json(&["run", "show", &run_id, "--no-reconcile", "--json"]);
    assert_eq!(shown["run"]["state"], "cancelled", "{shown}");
    let step_errors: Vec<String> = {
        let db = fixture.db();
        let mut statement = db
            .prepare("SELECT IFNULL(error_code, '-') FROM job_run_steps WHERE run_id = ?1")
            .unwrap();
        statement
            .query_map([&run_id], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    for code in ["worker_terminated", "terminal_outcome_conflict"] {
        assert!(
            !step_errors.iter().any(|error| error == code),
            "a cancelled worker's exit is not recorded as {code}: {step_errors:?}"
        );
    }

    let audit_rows = fixture.json(&["audit", "list", "--limit", "200", "--json"]);
    let audits: Vec<String> = audit_rows
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["target_id"] == run_id.as_str())
        .filter_map(|row| row["tool_name"].as_str().map(str::to_owned))
        .collect();
    for audit in [
        "pipeline.run.cancel.requested",
        "pipeline.run.cancel.worker_exit",
        "pipeline.run.cancel.completed",
    ] {
        assert!(
            audits.iter().any(|tool| tool == audit),
            "cancellation audit trail lacks {audit}: {audits:?}"
        );
    }
    let worker_exit = audit_rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| {
            row["target_id"] == run_id.as_str()
                && row["tool_name"] == "pipeline.run.cancel.worker_exit"
        })
        .expect("recorded worker exit");
    let exit: Value =
        serde_json::from_str(worker_exit["arguments_json"].as_str().unwrap()).unwrap();
    assert_eq!(exit["owner_pid"], worker_pid);
    assert_eq!(exit["signal"], libc::SIGTERM);
    assert!(
        exit["exit_status"]
            .as_str()
            .is_some_and(|status| !status.is_empty())
    );

    let repeated = fixture.json(&["run", "cancel", &run_id, "--confirm", "--json"]);
    assert_eq!(repeated["outcome"], "already_terminal");
    assert_eq!(repeated["final_state"], "cancelled");
    assert_eq!(repeated["signal_attempted"], false);
    assert_eq!(fixture.run_state(&run_id), "cancelled");
}

/// A stopped coordinator's workers retain workspace slots, including a
/// delivery whose wrapper has finished. A replacement records that inherited
/// occupancy without attributing those workers' outcomes to itself.
#[test]
fn replacement_drain_counts_and_reports_inherited_workers_until_they_finish() {
    if !isolated_run_observation(
        "run_observation::replacement_drain_counts_and_reports_inherited_workers_until_they_finish",
    ) {
        return;
    }
    use orbit_core::application::task::TaskAddParams;
    use orbit_core::{TaskComplexity, TaskStatus, TaskType};
    use orbit_types::workflow::{ChildDispatch, PipelineState};
    use serde_json::json;

    let fixture = Fixture::init();
    // The replacement's waves are classified against a pinned calm sample,
    // never the live host: a loaded test host would throttle the wave and
    // offer none of the work this test counts.
    let runtime =
        OrbitRuntime::from_roots(&fixture.home.join(".orbit"), &fixture.work.join(".orbit"))
            .unwrap()
            .with_host_resource_probe(std::sync::Arc::new(CalmHost));
    let tasks: Vec<_> = (0..7)
        .map(|index| {
            let file = format!("task-{index}.rs");
            fs::write(fixture.work.join(&file), "fixture\n").unwrap();
            runtime
                .add_task(TaskAddParams {
                    title: format!("independent task {index}"),
                    description: "Fixture delivery".into(),
                    acceptance_criteria: vec!["Delivery completes".into()],
                    plan: "Fixture plan".into(),
                    context_files: vec![format!("file:{file}")],
                    task_type: Some(TaskType::Feature),
                    complexity: TaskComplexity::Low,
                    status: Some(if index < 2 {
                        TaskStatus::InProgress
                    } else {
                        TaskStatus::Backlog
                    }),
                    ..Default::default()
                })
                .unwrap()
                .id
        })
        .collect();
    let sequence = std::cell::Cell::new(0);
    let seed = |job: &str, status: &str, input: Value, children: &[(&str, &str)]| {
        let index = sequence.get();
        sequence.set(index + 1);
        let id = format!("jrun-inherited-{index}");
        let now = chrono::Utc::now();
        fixture.db().execute(
            "INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,input_json,scheduled_at,started_at,created_at,pid) VALUES (?1,?2,?3,1,?4,?5,?6,?6,?6,?7)",
            params![id, fixture.workspace_id(), job, status, input.to_string(), now.to_rfc3339(), std::process::id()],
        ).unwrap();
        let mut state = PipelineState::new(id.clone(), job.into(), input);
        for (child, child_job) in children {
            state.record_child_dispatch(ChildDispatch::submitted(
                (*child).into(),
                (*child_job).into(),
                "dispatch".into(),
                false,
                false,
                now,
            ));
        }
        runtime.write_run_state(&id, &state).unwrap();
        id
    };
    let old_leaf = seed(
        "task_local_pipeline",
        "running",
        json!({"task_ids": [tasks[0]]}),
        &[],
    );
    let old_wrapper = seed(
        "task_auto_pipeline",
        "running",
        json!({"task_ids": [tasks[0]]}),
        &[(&old_leaf, "task_local_pipeline")],
    );
    let detached_leaf = seed(
        "task_pr_pipeline",
        "retrying",
        json!({"task_ids": [tasks[1]]}),
        &[],
    );
    let finished_wrapper = seed(
        "task_auto_pipeline",
        "success",
        json!({"task_ids": [tasks[1]]}),
        &[(&detached_leaf, "task_pr_pipeline")],
    );
    let old = seed(
        "workspace_auto_pipeline",
        "running",
        json!({"max_active_leaf_runs": 2}),
        &[
            (&old_wrapper, "task_auto_pipeline"),
            (&finished_wrapper, "task_auto_pipeline"),
        ],
    );
    fixture.json(&["run", "auto", "--stop", "--json"]);
    assert!(
        runtime
            .read_run_state(&old)
            .unwrap()
            .unwrap()
            .drain_admissions_stop
            .is_some()
    );
    // The stopped coordinator's loop ends, without cancelling either worker.
    fixture
        .db()
        .execute(
            "UPDATE job_runs SET state='success' WHERE run_id=?1",
            [&old],
        )
        .unwrap();
    let replacement = seed(
        "workspace_auto_pipeline",
        "running",
        json!({"max_active_leaf_runs": 3}),
        &[],
    );
    let classify = || {
        orbit_engine::RuntimeHost::run_deterministic(
            &runtime,
            "classify_workspace_auto_tasks",
            &json!({}),
            &json!({"run_id": replacement, "max_active_leaf_runs": 3, "mode": "pr"}),
            orbit_tools::ToolContext::default(),
        )
        .unwrap()
    };
    let verify = |occupied: u64, inherited: u64, offered: usize, limit: u64| {
        let wave = classify();
        assert_eq!(wave["active_leaf_runs"], occupied, "{wave:#}");
        assert_eq!(wave["inherited_leaf_runs"], inherited, "{wave:#}");
        assert_eq!(
            wave["free_slots"],
            limit.saturating_sub(occupied),
            "{wave:#}"
        );
        assert_eq!(
            wave["loose_task_ids"].as_array().unwrap().len(),
            offered,
            "{wave:#}"
        );
        let shown = fixture.json(&["run", "show", &replacement, "--no-reconcile", "--json"]);
        let capacity = json!({"active_leaf_runs": occupied, "inherited_leaf_runs": inherited, "max_active_leaf_runs": limit});
        assert_eq!(
            shown["pipeline_state"]["drain_last_pass"]["capacity"],
            capacity
        );
        assert_eq!(shown["drain_summary"]["capacity"], capacity);
        let output = fixture
            .orbit()
            .args(["run", "show", &replacement, "--no-reconcile"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            text.contains(&format!(
                "occupied={occupied} inherited={inherited} limit={limit}"
            )),
            "{text}"
        );
        wave
    };
    // A ceiling below inherited occupancy must offer no new work.
    fixture.json(&["run", "concurrency", &replacement, "--set", "1", "--json"]);
    verify(2, 2, 0, 1);
    fixture.json(&["run", "concurrency", &replacement, "--set", "3", "--json"]);
    let wave = verify(2, 2, 1, 3);
    let own_task = wave["loose_task_ids"][0].as_str().unwrap();
    let own_leaf = seed(
        "task_local_pipeline",
        "running",
        json!({"task_ids": [own_task]}),
        &[],
    );
    let own_wrapper = seed(
        "task_auto_pipeline",
        "running",
        json!({"task_ids": [own_task]}),
        &[(&own_leaf, "task_local_pipeline")],
    );
    let mut state = runtime.read_run_state(&replacement).unwrap().unwrap();
    state.record_child_dispatch(ChildDispatch::submitted(
        own_wrapper.clone(),
        "task_auto_pipeline".into(),
        "dispatch".into(),
        false,
        false,
        chrono::Utc::now(),
    ));
    runtime.write_run_state(&replacement, &state).unwrap();
    verify(3, 2, 0, 3);
    // Even if our wrapper ends first, its live descendant is ours, not inherited.
    fixture
        .db()
        .execute(
            "UPDATE job_runs SET state='success' WHERE run_id=?1",
            [&own_wrapper],
        )
        .unwrap();
    verify(3, 2, 0, 3);
    for id in [&old_wrapper, &old_leaf] {
        fixture
            .db()
            .execute("UPDATE job_runs SET state='success' WHERE run_id=?1", [id])
            .unwrap();
    }
    verify(2, 1, 1, 3);
    fixture
        .db()
        .execute(
            "UPDATE job_runs SET state='success' WHERE run_id=?1",
            [&detached_leaf],
        )
        .unwrap();
    verify(1, 0, 2, 3);
    let shown = fixture.json(&["run", "show", &replacement, "--no-reconcile", "--json"]);
    assert_eq!(shown["drain_summary"]["admitted"], 1);
    assert_eq!(shown["drain_summary"]["succeeded"], 1);
    assert_eq!(shown["drain_summary"]["failed"], 0);
}

/// Worker-limit adjustment only writes a drain control record. The fixture's
/// live test PID prevents orphan reconciliation; this record is never passed
/// to cancellation or any process-control command.
#[test]
fn run_concurrency_updates_persisted_revision_and_refuses_stale_writes() {
    let fixture = Fixture::init();
    let runtime = orbit_core::OrbitRuntime::from_roots(
        &fixture.home.join(".orbit"),
        &fixture.work.join(".orbit"),
    )
    .unwrap();
    let id = "jrun-cli-workers";
    let input = serde_json::json!({"max_active_leaf_runs": 2});
    let now = chrono::Utc::now().to_rfc3339();
    fixture.db().execute(
        "INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,input_json,scheduled_at,started_at,created_at,pid) VALUES (?1,?2,'workspace_auto_pipeline',1,'running',?3,?4,?4,?4,?5)",
        params![id,fixture.workspace_id(),input.to_string(),now,std::process::id()],
    ).unwrap();
    runtime
        .write_run_state(
            id,
            &orbit_types::workflow::PipelineState::new(
                id.into(),
                "workspace_auto_pipeline".into(),
                input.clone(),
            ),
        )
        .unwrap();
    let changed = fixture.json(&[
        "run",
        "concurrency",
        id,
        "--set",
        "3",
        "--if-revision",
        "0",
        "--reason",
        "CLI fixture",
        "--json",
    ]);
    assert_eq!(changed["outcome"], "updated");
    assert_eq!(changed["previous_concurrency"], 2);
    assert_eq!(changed["concurrency"], 3);
    assert_eq!(changed["revision"], 1);
    let shown = fixture.json(&["run", "show", id, "--no-reconcile", "--json"]);
    assert_eq!(shown["run"]["state"], "running");
    let before = runtime.read_run_state(id).unwrap().unwrap();
    let limit = before
        .drain_worker_limit
        .as_ref()
        .expect("persisted CLI worker limit");
    assert_eq!(limit.max_active_leaf_runs, 3);
    assert_eq!(limit.previous_max_active_leaf_runs, 2);
    assert_eq!(limit.revision, 1);
    assert_eq!(limit.actor, "cli");
    assert_eq!(limit.reason.as_deref(), Some("CLI fixture"));
    let unchanged = fixture.json(&[
        "run",
        "concurrency",
        id,
        "--set",
        "3",
        "--if-revision",
        "1",
        "--json",
    ]);
    assert_eq!(unchanged["outcome"], "unchanged");
    assert_eq!(unchanged["revision"], 1);
    let error = fixture.failure(&[
        "run",
        "concurrency",
        id,
        "--set",
        "4",
        "--if-revision",
        "0",
        "--json",
    ]);
    assert!(
        error["error"].as_str().unwrap().contains("revision"),
        "{error}"
    );
    assert_eq!(runtime.read_run_state(id).unwrap().unwrap(), before);
    assert_eq!(runtime.show_job_run(id).unwrap().input, Some(input));
    assert_eq!(fixture.run_state(id), "running");
}

/// A drain's worker limit is the only ceiling on the delivery jobs it
/// dispatches. With nineteen delivery wrappers already running — past the ten
/// the job once allowed — a twentieth is admitted to run at once instead of
/// waiting `pending` behind a job-level limit while it holds a drain slot. The
/// detached worker is substituted, so nothing beyond admission executes.
#[test]
fn a_drain_at_concurrency_twenty_runs_twenty_delivery_wrappers_at_once() {
    let fixture = Fixture::init();
    orbit_core::test_support::install_substitute_pipeline_worker(["sh", "-c", "exit 0"]);
    let runtime = orbit_core::OrbitRuntime::from_roots(
        &fixture.home.join(".orbit"),
        &fixture.work.join(".orbit"),
    )
    .unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    for index in 0..19 {
        fixture.db().execute(
            "INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,scheduled_at,started_at,created_at,pid) VALUES (?1,?2,'task_auto_pipeline',1,'running',?3,?3,?3,?4)",
            params![format!("jrun-cli-wrapper-{index}"),fixture.workspace_id(),now,std::process::id()],
        ).unwrap();
    }

    let twentieth = runtime
        .submit_pipeline_run(
            "task_auto_pipeline",
            serde_json::json!({"task_ids": []}),
            None,
            Some("fixture"),
        )
        .unwrap();

    assert!(!twentieth.queued, "{twentieth:?}");
    assert_eq!(twentieth.queue_position, None);
}

/// `orbit run concurrency` retunes a replica's pull drain the same way it
/// does an auto drain, to any positive ceiling, and still refuses a run that
/// is not a drain.
#[test]
fn run_concurrency_retunes_a_pull_drain_past_the_former_leaf_ceiling() {
    let fixture = Fixture::init();
    let runtime = orbit_core::OrbitRuntime::from_roots(
        &fixture.home.join(".orbit"),
        &fixture.work.join(".orbit"),
    )
    .unwrap();
    let id = "jrun-cli-pull-workers";
    let input = serde_json::json!({"max_active_leaf_runs": 5});
    let now = chrono::Utc::now().to_rfc3339();
    fixture.db().execute(
        "INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,input_json,scheduled_at,started_at,created_at,pid) VALUES (?1,?2,'workspace_pull_pipeline',1,'running',?3,?4,?4,?4,?5)",
        params![id,fixture.workspace_id(),input.to_string(),now,std::process::id()],
    ).unwrap();
    runtime
        .write_run_state(
            id,
            &orbit_types::workflow::PipelineState::new(
                id.into(),
                "workspace_pull_pipeline".into(),
                input.clone(),
            ),
        )
        .unwrap();

    let changed = fixture.json(&["run", "concurrency", id, "--set", "24", "--json"]);

    assert_eq!(changed["outcome"], "updated");
    assert_eq!(changed["job_id"], "workspace_pull_pipeline");
    assert_eq!(changed["previous_concurrency"], 5);
    assert_eq!(changed["concurrency"], 24);
    let state = runtime.read_run_state(id).unwrap().unwrap();
    assert_eq!(state.effective_max_active_leaf_runs(5), 24);
    assert_eq!(runtime.show_job_run(id).unwrap().input, Some(input));

    let refused = fixture.failure(&["run", "concurrency", FAILED, "--set", "24", "--json"]);
    assert!(
        refused["error"].as_str().unwrap().contains("fixture_job"),
        "{refused}"
    );
}

/// [ORB-13901] A short-lived CLI cannot sample long enough to judge
/// sustained pressure, so it reports the throttle a live drain recorded on
/// its last pass: readiness and `run show` name the resource, value,
/// threshold and since-when, and ship discovery stands down.
#[test]
fn a_live_drains_recorded_throttle_reaches_readiness_run_show_and_ship() {
    let fixture = Fixture::init();
    let runtime = orbit_core::OrbitRuntime::from_roots(
        &fixture.home.join(".orbit"),
        &fixture.work.join(".orbit"),
    )
    .unwrap();
    let id = "jrun-cli-throttled-pull";
    let now = chrono::Utc::now();
    fixture.db().execute(
        "INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,input_json,scheduled_at,started_at,created_at,pid) VALUES (?1,?2,'workspace_pull_pipeline',1,'running','{}',?3,?3,?3,?4)",
        params![id,fixture.workspace_id(),now.to_rfc3339(),std::process::id()],
    ).unwrap();
    let since = chrono::DateTime::parse_from_rfc3339("2026-10-04T08:41:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let mut state = orbit_types::workflow::PipelineState::new(
        id.into(),
        "workspace_pull_pipeline".into(),
        serde_json::json!({}),
    );
    state.drain_last_pass = Some(orbit_types::workflow::DrainAdmissionPass {
        capacity: None,
        recorded_at: now,
        queued: 0,
        deferred: Vec::new(),
        excluded: Vec::new(),
        excluded_total: 0,
        waiting_recorded_at: None,
        waiting_by_reason: Default::default(),
        consecutive_idle_passes: 0,
        last_pass_error_code: None,
        last_pass_error: None,
        consecutive_pass_failures: 0,
        degraded: false,
        resource_throttle: Some(orbit_types::workflow::ResourceThrottle {
            resources: vec![
                orbit_types::workflow::ResourcePressure {
                    resource: "memory".into(),
                    percent: 93.2,
                    high_percent: 90,
                    resume_percent: 80,
                    since,
                },
                orbit_types::workflow::ResourcePressure {
                    resource: "cpu".into(),
                    percent: 89.0,
                    high_percent: 90,
                    resume_percent: 75,
                    since,
                },
            ],
        }),
    });
    runtime.write_run_state(id, &state).unwrap();
    let held = "Admissions throttled: memory 93% (throttled at \u{2265} 90% since 2026-10-04 08:41Z; resumes below 80%); cpu 89% (throttled at \u{2265} 90% since 2026-10-04 08:41Z; resumes below 75%)";

    let readiness = fixture.json(&["run", "readiness", "--json"]);
    assert_eq!(readiness["capacity"]["free_slots"], 0, "{readiness:#}");
    assert_eq!(
        readiness["capacity"]["resource_throttle"]["resources"][0]["percent"],
        93.2
    );
    assert_eq!(
        readiness["capacity"]["resource_throttle"]["resources"][1]["percent"],
        89.0
    );
    let text = fixture.orbit().args(["run", "readiness"]).output().unwrap();
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains(held), "{text}");
    assert!(
        !text.contains("89% \u{2265}"),
        "reading inside hysteresis band must not print a false comparison: {text}"
    );

    let shown = fixture
        .orbit()
        .args(["run", "show", id, "--no-reconcile"])
        .output()
        .unwrap();
    let shown = String::from_utf8_lossy(&shown.stdout);
    assert!(shown.contains(&format!("Throttled: {held}")), "{shown}");

    let refused = fixture.failure(&["run", "ship", "--json"]);
    assert!(
        refused["error"]
            .as_str()
            .is_some_and(|error| error.contains("resource_throttled") && error.contains(held)),
        "{refused}"
    );
}

/// [ORB-14475] A pull drain's recorded owner answer reaches `run show` as
/// the same `Still waiting` lines a local drain prints: each kept-off task
/// with its reason and the tasks it waits on, the age of the owner's answer,
/// and, once several passes in a row claimed nothing, one line saying why.
#[test]
fn run_show_names_the_tasks_a_pull_drains_owner_kept_off_this_host() {
    const CHILD: &str = "ORBIT_TEST_PULL_WAITING_CHILD";
    const TEST: &str =
        "run_observation::run_show_names_the_tasks_a_pull_drains_owner_kept_off_this_host";
    use orbit_types::workflow::{DrainAdmissionPass, DrainWaitingTask};
    if std::env::var(CHILD).as_deref() != Ok("1") {
        let home = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .current_dir(home.path());
        let output = orbit_common::process::run_bounded_capped(
            &mut command,
            std::time::Duration::from_secs(30),
            64 * 1024,
        )
        .unwrap();
        test_env::assert_child_test_passed(TEST, output.status, output.stdout, output.stderr);
        return;
    }
    let fixture = Fixture::init();
    let runtime = orbit_core::OrbitRuntime::from_roots(
        &fixture.home.join(".orbit"),
        &fixture.work.join(".orbit"),
    )
    .unwrap();
    let id = "jrun-cli-pull-waiting";
    let now = chrono::Utc::now();
    let answered = chrono::DateTime::parse_from_rfc3339("2026-10-07T06:44:53Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    fixture.db().execute(
        "INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,input_json,scheduled_at,started_at,created_at,pid) VALUES (?1,?2,'workspace_pull_pipeline',1,'running','{}',?3,?3,?3,?4)",
        params![id, fixture.workspace_id(), now.to_rfc3339(), std::process::id()],
    ).unwrap();
    let waiting =
        |task: &str, reason: &str, blocked_by: &[&str], detail: Option<&str>| DrainWaitingTask {
            task_id: task.into(),
            reason: Some(reason.into()),
            blocked_by: blocked_by.iter().map(ToString::to_string).collect(),
            detail: detail.map(ToString::to_string),
        };
    let mut state = orbit_types::workflow::PipelineState::new(
        id.into(),
        "workspace_pull_pipeline".into(),
        serde_json::json!({}),
    );
    state.drain_last_pass = Some(DrainAdmissionPass {
        capacity: None,
        recorded_at: now,
        queued: 9,
        deferred: vec![
            waiting("ORB-101", "context_lock_conflict", &["ORB-900"], None),
            waiting(
                "ORB-102",
                "owner_hold",
                &[],
                Some("held for a red base: make ci-lint"),
            ),
        ],
        excluded: vec![
            waiting("ORB-103", "dependency_not_done", &["ORB-901"], None),
            waiting(
                "ORB-104",
                "host_os_mismatch",
                &[],
                Some("waits for a linux host (os:linux); the executor runs macos"),
            ),
            waiting(
                "ORB-105",
                "crew_unavailable",
                &[],
                Some("crew antigravity cannot run on this host"),
            ),
        ],
        excluded_total: 3,
        waiting_recorded_at: Some(answered),
        waiting_by_reason: [
            ("context_lock_conflict", 4),
            ("owner_hold", 11),
            ("dependency_not_done", 1),
            ("host_os_mismatch", 1),
            ("crew_unavailable", 1),
        ]
        .into_iter()
        .map(|(reason, count)| (reason.to_string(), count))
        .collect(),
        consecutive_idle_passes: 4,
        resource_throttle: None,
        last_pass_error_code: None,
        last_pass_error: None,
        consecutive_pass_failures: 0,
        degraded: false,
    });
    runtime.write_run_state(id, &state).unwrap();

    let shown = fixture.json(&["run", "show", id, "--no-reconcile", "--json"]);
    let pass = &shown["pipeline_state"]["drain_last_pass"];
    assert_eq!(pass["queued"], 9, "{pass}");
    assert_eq!(pass["excluded_total"], 3, "{pass}");
    assert_eq!(pass["deferred"][0]["blocked_by"][0], "ORB-900", "{pass}");
    assert_eq!(
        pass["excluded"][0]["reason"], "dependency_not_done",
        "{pass}"
    );
    assert_eq!(
        pass["waiting_recorded_at"], "2026-10-07T06:44:53Z",
        "{pass}"
    );

    let output = fixture
        .orbit()
        .args(["run", "show", id, "--no-reconcile"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    for expected in [
        "Still waiting: 9 admissible and 3 excluded backlog task(s) were never started at the last pass (the owner answered 2026-10-07 06:44:53Z)",
        "Task ORB-101: context_lock_conflict blocked-by=ORB-900",
        "Task ORB-102: owner_hold (held for a red base: make ci-lint)",
        "Task ORB-103: dependency_not_done blocked-by=ORB-901",
        "Task ORB-104: host_os_mismatch (waits for a linux host (os:linux); the executor runs macos)",
        "Task ORB-105: crew_unavailable (crew antigravity cannot run on this host)",
        "idle: 18 backlog task(s) kept off this host for 4 consecutive passes (11 held on the owner, 4 footprint holds,",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
    }

    // Fewer than several idle passes: the per-task lines stand alone.
    state
        .drain_last_pass
        .as_mut()
        .unwrap()
        .consecutive_idle_passes = 1;
    runtime.write_run_state(id, &state).unwrap();
    let output = fixture
        .orbit()
        .args(["run", "show", id, "--no-reconcile"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("Task ORB-101:"), "{text}");
    assert!(!text.contains("idle:"), "{text}");
}

/// A durable failed-pass record is visible through the actual CLI in text
/// and JSON, independently of successful coordinator step outputs.
#[test]
fn run_show_exposes_degraded_pull_pass_health() {
    const CHILD: &str = "ORBIT_TEST_PULL_PASS_HEALTH_CHILD";
    const TEST: &str = "run_observation::run_show_exposes_degraded_pull_pass_health";
    if std::env::var(CHILD).as_deref() != Ok("1") {
        let home = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .current_dir(home.path());
        let output = orbit_common::process::run_bounded_capped(
            &mut command,
            std::time::Duration::from_secs(30),
            64 * 1024,
        )
        .unwrap();
        test_env::assert_child_test_passed(TEST, output.status, output.stdout, output.stderr);
        return;
    }
    let fixture = Fixture::init();
    let runtime = orbit_core::OrbitRuntime::from_roots(
        &fixture.home.join(".orbit"),
        &fixture.work.join(".orbit"),
    )
    .unwrap();
    let id = "jrun-cli-degraded-pull";
    let now = chrono::Utc::now();
    fixture.db().execute(
        "INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,input_json,scheduled_at,started_at,created_at,pid) VALUES (?1,?2,'workspace_pull_pipeline',1,'running','{}',?3,?3,?3,?4)",
        params![id, fixture.workspace_id(), now.to_rfc3339(), std::process::id()],
    ).unwrap();
    let mut state = orbit_types::workflow::PipelineState::new(
        id.into(),
        "workspace_pull_pipeline".into(),
        serde_json::json!({}),
    );
    state.drain_last_pass = Some(orbit_types::workflow::DrainAdmissionPass {
        capacity: None,
        recorded_at: now,
        queued: 0,
        deferred: Vec::new(),
        excluded: Vec::new(),
        excluded_total: 0,
        waiting_recorded_at: None,
        waiting_by_reason: Default::default(),
        consecutive_idle_passes: 0,
        resource_throttle: None,
        last_pass_error_code: None,
        last_pass_error: Some("protocol_mismatch: caller revision 2; owner revision 1".into()),
        consecutive_pass_failures: 3,
        degraded: true,
    });
    runtime.write_run_state(id, &state).unwrap();
    let shown = fixture.json(&["run", "show", id, "--no-reconcile", "--json"]);
    let pass = &shown["pipeline_state"]["drain_last_pass"];
    assert_eq!(pass["consecutive_pass_failures"], 3);
    assert_eq!(pass["degraded"], true);
    assert_eq!(
        pass["last_pass_error"],
        "protocol_mismatch: caller revision 2; owner revision 1"
    );
    let output = fixture
        .orbit()
        .args(["run", "show", id, "--no-reconcile"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("consecutive_failures=3"), "{text}");
    assert!(
        text.contains("last_pass_error=protocol_mismatch:"),
        "{text}"
    );
    assert!(text.contains("Drain degraded:"), "{text}");
    let pass = state.drain_last_pass.as_mut().unwrap();
    pass.last_pass_error_code = Some("protocol_skew".into());
    pass.last_pass_error =
        Some("protocol_skew: caller and owner request fingerprints differ".into());
    let skew_message = pass.last_pass_error.clone();
    runtime.write_run_state(id, &state).unwrap();
    fixture
        .db()
        .execute(
            "INSERT INTO job_run_steps (workspace_id, run_id, step_index, target_type,
            target_id, state, started_at, finished_at, error_code, error_message)
         VALUES (?1, ?2, 0, 'job', 'workspace_pull_pipeline', 'failed', ?3, ?3,
            'protocol_skew', ?4)",
            params![fixture.workspace_id(), id, now.to_rfc3339(), skew_message],
        )
        .unwrap();
    orbit_engine::RuntimeHost::finalize_job_run(
        &runtime,
        id,
        orbit_types::workflow::JobRunState::Failed,
        now,
        None,
    )
    .unwrap();
    // The fixture intentionally has no provider CLIs, so other doctor rows
    // fail and its process exits nonzero. Inspect the real JSON diagnostic.
    let diagnosis = fixture.orbit().args(["doctor", "--json"]).output().unwrap();
    let diagnosed: Value = serde_json::from_slice(&diagnosis.stdout).unwrap();
    let row = diagnosed
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["check"] == "pull-protocol")
        .unwrap();
    assert_eq!(row["status"], "warning", "{diagnosed}");
    assert!(
        row["message"].as_str().unwrap().contains("protocol_skew"),
        "{row}"
    );
    assert!(
        row["remediation"].as_str().unwrap().contains("restart"),
        "{row}"
    );
}

#[cfg(unix)]
mod agent_invoke;

/// A partial forced cancellation keeps its structured result on stdout and
/// exits unsuccessfully in both JSON and terminal modes.
#[test]
fn force_cancel_reports_unstopped_local_children_and_exits_one() {
    const MARKER: &str = "ORBIT_TEST_LOCAL_CANCEL_CHILD";
    if std::env::var_os(MARKER).is_none() {
        let home = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        test_env::clear_inherited_authority(|key| {
            command.env_remove(key);
        });
        let output = command
            .args([
                "--exact",
                "run_observation::force_cancel_reports_unstopped_local_children_and_exits_one",
                "--nocapture",
            ])
            .env(MARKER, "1")
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .current_dir(home.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        return;
    }
    for as_json in [true, false] {
        let fixture = Fixture::init();
        let runtime = orbit_core::OrbitRuntime::from_roots(
            &fixture.home.join(".orbit"),
            &fixture.work.join(".orbit"),
        )
        .unwrap();
        let drain = "jrun-cli-force-local";
        let failed = "jrun-cli-unstopped-child";
        let stopped = "jrun-cli-stopped-child";
        let now = chrono::Utc::now();
        for (id, job, state) in [
            (drain, "workspace_auto_pipeline", "pending"),
            (failed, "task_auto_pipeline", "running"),
            (stopped, "task_auto_pipeline", "pending"),
        ] {
            // A live, unverified PID prevents orphan reconciliation. Pending
            // runs need no signal; the running child cannot be confirmed gone.
            fixture.db().execute(
                "INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,scheduled_at,started_at,created_at,pid) VALUES (?1,?2,?3,1,?4,?5,?5,?5,?6)",
                params![id, fixture.workspace_id(), job, state, now.to_rfc3339(), std::process::id()],
            ).unwrap();
        }
        let mut state = orbit_types::workflow::PipelineState::new(
            drain.into(),
            "workspace_auto_pipeline".into(),
            serde_json::json!({}),
        );
        for child in [failed, stopped] {
            state.record_child_dispatch(orbit_types::workflow::ChildDispatch::submitted(
                child.into(),
                "task_auto_pipeline".into(),
                "dispatch".into(),
                false,
                false,
                now,
            ));
        }
        runtime.write_run_state(drain, &state).unwrap();
        let mut command = fixture.orbit();
        command.args(["run", "cancel", drain, "--confirm", "--force"]);
        if as_json {
            command.arg("--json");
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        if as_json {
            let body: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(body["outcome"], "cancelled");
            assert_eq!(body["final_state"], "cancelled");
            assert_eq!(body["forced_runs"], serde_json::json!([stopped]));
            assert_eq!(body["unstopped_children"][0]["child_run_id"], failed);
            assert!(
                body["unstopped_children"][0]["reason"]
                    .as_str()
                    .unwrap()
                    .contains("could not confirm")
            );
        } else {
            let text = String::from_utf8_lossy(&output.stdout);
            for value in [drain, stopped, failed, "could not confirm"] {
                assert!(text.contains(value), "missing {value}: {text}");
            }
        }
        assert_eq!(fixture.run_state(drain), "cancelled");
        assert_eq!(fixture.run_state(stopped), "cancelled");
        assert_eq!(fixture.run_state(failed), "running");
    }
}

/// A surface reservation is visible where an operator looks for a waiting
/// task [ORB-14310]: `run readiness` and the drain's `run show` both name the
/// typed `surface_reserved` reason and the reserving task, from a real drain
/// pass rather than a seeded record.
#[test]
fn a_surface_reservation_reaches_readiness_and_drain_run_show() {
    const CHILD: &str = "ORBIT_TEST_SURFACE_RESERVATION_CHILD";
    const TEST: &str =
        "run_observation::a_surface_reservation_reaches_readiness_and_drain_run_show";
    if std::env::var(CHILD).as_deref() != Ok("1") {
        let home = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .current_dir(home.path());
        let output = orbit_common::process::run_bounded_capped(
            &mut command,
            std::time::Duration::from_secs(60),
            64 * 1024,
        )
        .unwrap();
        test_env::assert_child_test_passed(TEST, output.status, output.stdout, output.stderr);
        return;
    }
    use orbit_core::application::task::TaskAddParams;
    use orbit_core::{TaskComplexity, TaskPriority, TaskStatus, TaskType};

    let fixture = Fixture::init();
    let runtime = orbit_core::OrbitRuntime::from_roots(
        &fixture.home.join(".orbit"),
        &fixture.work.join(".orbit"),
    )
    .unwrap();
    for file in ["held.rs", "shared.rs"] {
        fs::write(fixture.work.join(file), "fixture\n").unwrap();
    }
    let add = |title: &str, status, priority, files: &[&str]| {
        runtime
            .add_task(TaskAddParams {
                title: title.to_string(),
                description: format!("Fixture task: {title}"),
                acceptance_criteria: vec!["Fixture task is observable.".to_string()],
                plan: "Fixture plan.".to_string(),
                context_files: files.iter().map(|file| format!("file:{file}")).collect(),
                priority,
                complexity: TaskComplexity::Medium,
                task_type: Some(TaskType::Feature),
                status: Some(status),
                ..Default::default()
            })
            .unwrap()
            .id
    };
    add(
        "holder",
        TaskStatus::InProgress,
        TaskPriority::Medium,
        &["held.rs"],
    );
    let critical = add(
        "critical",
        TaskStatus::Backlog,
        TaskPriority::Critical,
        &["held.rs", "shared.rs"],
    );
    let withheld = add(
        "withheld",
        TaskStatus::Backlog,
        TaskPriority::Low,
        &["shared.rs"],
    );

    let drain = "jrun-cli-surface-reservation";
    let now = chrono::Utc::now();
    fixture.db().execute(
        "INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,input_json,scheduled_at,started_at,created_at,pid) VALUES (?1,?2,'workspace_auto_pipeline',1,'running','{}',?3,?3,?3,?4)",
        params![drain, fixture.workspace_id(), now.to_rfc3339(), std::process::id()],
    ).unwrap();
    runtime
        .write_run_state(
            drain,
            &orbit_types::workflow::PipelineState::new(
                drain.into(),
                "workspace_auto_pipeline".into(),
                serde_json::json!({}),
            ),
        )
        .unwrap();
    let wave = orbit_engine::RuntimeHost::run_deterministic(
        &runtime,
        "classify_workspace_auto_tasks",
        &serde_json::json!({}),
        &serde_json::json!({"run_id": drain, "max_active_leaf_runs": 2}),
        orbit_tools::ToolContext::default(),
    )
    .unwrap();
    assert_eq!(wave["loose_task_ids"], serde_json::json!([]), "{wave:#}");

    let readiness = fixture.json(&["run", "readiness", "--json"]);
    let entry = readiness["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["task_id"] == withheld.as_str())
        .expect("withheld task in readiness");
    assert_eq!(entry["reason"], "surface_reserved", "{readiness:#}");
    assert_eq!(entry["blocking_task_ids"], serde_json::json!([critical]));
    let text = fixture.orbit().args(["run", "readiness"]).output().unwrap();
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.contains(&format!(
            "{withheld}: waiting (surface_reserved) blocked-by={critical}"
        )),
        "{text}"
    );

    let shown = fixture
        .orbit()
        .args(["run", "show", drain, "--no-reconcile"])
        .output()
        .unwrap();
    assert!(shown.status.success());
    let shown = String::from_utf8_lossy(&shown.stdout);
    assert!(
        shown.contains(&format!(
            "Task {withheld}: surface_reserved blocked-by={critical}"
        )),
        "{shown}"
    );
}

/// A live agent whose supervisor reported a descendant stopped past its
/// threshold, which is still stopped, reads as blocked rather than plainly
/// alive; the same agent without that report reads plainly alive. The agent
/// and its stopped child are real processes so both liveness probes answer.
#[cfg(target_os = "linux")]
#[test]
fn run_show_marks_an_agent_blocked_on_a_stopped_descendant() {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    use orbit_common::process::identity::{linux_process_stat, process_start_identity_token};

    const BLOCKED: &str = "jrun-20261008-0100";
    const ALIVE: &str = "jrun-20261008-0200";

    /// The agent's process group, killed and reaped on drop.
    struct AgentTree(std::process::Child);

    impl Drop for AgentTree {
        fn drop(&mut self) {
            // SAFETY: signals only the fixture's own unreaped process group.
            unsafe { libc::killpg(self.0.id() as libc::pid_t, libc::SIGKILL) };
            let _ = self.0.wait();
        }
    }

    let fixture = Fixture::init();
    let pid_file = fixture.home.join("stopped-child.pid");
    let mut agent = std::process::Command::new("/bin/sh");
    agent
        .arg("-c")
        .arg(format!(
            "sh -c 'kill -STOP $$' & echo $! > '{}'; wait",
            pid_file.display()
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let agent = AgentTree(agent.spawn().expect("spawn agent"));
    let agent_pid = agent.0.id();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let child_pid = loop {
        let child = fs::read_to_string(&pid_file)
            .ok()
            .and_then(|pid| pid.trim().parse::<u32>().ok())
            .filter(|pid| linux_process_stat(*pid).is_some_and(|stat| stat.state == 'T'));
        match child {
            Some(pid) => break pid,
            None if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            None => panic!("the agent's child never stopped"),
        }
    };

    let workspace_id = fixture.workspace_id();
    let db = fixture.db();
    let now = chrono::Utc::now().to_rfc3339();
    let mut events = Vec::new();
    for run_id in [BLOCKED, ALIVE] {
        db.execute(
            "INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state,
                 scheduled_at, started_at, created_at)
             VALUES (?1, ?2, 'task_pr_pipeline', 1, 'running', ?3, ?3, ?3)",
            params![run_id, workspace_id, now],
        )
        .expect("seed running leaf");
        events.push((
            run_id,
            serde_json::json!({
                "event_id": format!("{run_id}-process"),
                "parent_event_id": format!("{run_id}-invocation"),
                "body_kind": "cli_invocation_process",
                "provider": "antigravity",
                "pid": agent_pid,
                "pid_start_time": process_start_identity_token(agent_pid),
            }),
        ));
    }
    events.push((
        BLOCKED,
        serde_json::json!({
            "event_id": format!("{BLOCKED}-stopped"),
            "parent_event_id": format!("{BLOCKED}-invocation"),
            "body_kind": "cli_invocation_stopped_descendant",
            "provider": "antigravity",
            "pid": child_pid,
            "pid_start_time": process_start_identity_token(child_pid),
            "command": "sh -c kill -STOP $$",
            "stopped_ms": 612_000,
            "ended": false,
            "error": "Operation not permitted (os error 1)",
        }),
    ));
    for (run_id, mut event) in events {
        event["ts"] = Value::String(now.clone());
        event["run_id"] = Value::String(run_id.to_string());
        event["step_id"] = Value::String("implement_one".to_string());
        db.execute(
            "INSERT INTO v2_audit_events (workspace_id, event_id, source, schema_version,
                 event_type, ts, run_id, agent_identity, parent_event_id, payload_json)
             VALUES (?1, ?2, 'v2_envelope', 1, 'activity.progress', ?3, ?4, 'test', ?5, ?6)",
            params![
                workspace_id,
                event["event_id"].as_str().unwrap(),
                now,
                run_id,
                event["parent_event_id"].as_str().unwrap(),
                event.to_string(),
            ],
        )
        .expect("seed audit event");
    }

    let text = |run_id: &str| {
        let output = fixture
            .orbit()
            .args(["run", "show", run_id, "--no-reconcile"])
            .output()
            .expect("spawn orbit");
        assert!(output.status.success(), "{output:?}");
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    let agent_line = |shown: &str| {
        shown
            .lines()
            .find(|line| line.starts_with("Agent:"))
            .unwrap_or_else(|| panic!("no Agent: line in {shown}"))
            .to_string()
    };

    let blocked = text(BLOCKED);
    assert!(
        agent_line(&blocked).contains(&format!(
            "pid={agent_pid} step=implement_one liveness=alive blocked=stopped-descendant "
        )),
        "{blocked}"
    );
    assert!(
        blocked.contains(&format!(
            "  stopped descendant: pid={child_pid} command=`sh -c kill -STOP $$` stopped for at least 612s; the supervisor could not end it (Operation not permitted (os error 1)); still stopped"
        )),
        "{blocked}"
    );
    let json = fixture.json(&["run", "show", BLOCKED, "--no-reconcile", "--json"]);
    let process = &json["provider_processes"][0];
    assert_eq!(process["liveness"], "alive");
    assert_eq!(process["blocked_on_stopped_descendant"]["pid"], child_pid);
    assert_eq!(
        process["blocked_on_stopped_descendant"]["still_stopped"],
        true
    );
    assert_eq!(process["stopped_descendants"][0]["ended"], false);

    let alive = text(ALIVE);
    let line = agent_line(&alive);
    assert!(
        line.contains("liveness=alive started_at=") && !line.contains("blocked="),
        "{alive}"
    );
    let json = fixture.json(&["run", "show", ALIVE, "--no-reconcile", "--json"]);
    assert!(json["provider_processes"][0]["blocked_on_stopped_descendant"].is_null());
    drop(agent);
}
