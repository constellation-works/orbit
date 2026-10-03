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

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use rusqlite::{Connection, params};
use serde_json::Value;

const STALE_RUNNING: &str = "jrun-20260920-0100";
const STALE_PENDING: &str = "jrun-20260920-0200";
const FAILED: &str = "jrun-20260920-0300";

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
        fs::create_dir_all(&work).expect("fixture work");
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
            .env("USERPROFILE", &self.home);
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

    fn json(&self, args: &[&str]) -> Value {
        let output = self.orbit().args(args).output().expect("spawn orbit");
        assert!(
            output.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if std::env::var_os("ORBIT_QA_TRACE_CLI").is_some() {
            eprintln!(
                "QA_CLI {}",
                serde_json::json!({
                    "test": std::thread::current().name(), "argv": args,
                    "exit_code": output.status.code(),
                })
            );
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
