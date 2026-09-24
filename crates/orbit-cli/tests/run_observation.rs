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
        serde_json::from_slice(&output.stdout).expect("json output")
    }
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Every read form the run-failure scan permits, including the latest-run
/// defaults, leaves the stale runs and their reservations exactly as stored
/// while still returning the finished-run and step-failure evidence.
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
