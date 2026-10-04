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

use crate::{fixture_crew, git_repo};

use std::fs;
use std::io::Write as _;
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

/// Capture the pre-change stable timestamp independently of Orbit's probe.
#[cfg(target_os = "linux")]
fn ps_lstart_utc(pid: u32) -> String {
    let output = std::process::Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .env("TZ", "UTC")
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .output()
        .expect("capture pre-change ps fixture");
    assert!(output.status.success(), "ps fixture: {output:?}");
    let raw = String::from_utf8(output.stdout).unwrap().trim().to_string();
    assert!(!raw.is_empty(), "ps must describe the live fixture process");
    raw
}

/// Re-execution clears PATH only in the child, without changing the parallel
/// test runner's environment. The fixture token comes from the old ps probe.
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
    let raw = ps_lstart_utc(pid);
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
#[cfg(target_os = "linux")]
#[test]
fn cancelling_owners_with_pre_change_ps_tokens_still_signals_them() {
    use orbit_common::process::identity::{
        STABLE_TOKEN_PREFIX, STABLE_TOKEN_PREFIX_V1, current_pid_namespace, process_is_alive,
    };

    let fixture = Fixture::init();
    for format in ["v2", "v1", "legacy"] {
        let owner = test_env::spawn_unrelated_process();
        let raw = ps_lstart_utc(owner.pid());
        let token = match format {
            "v2" => format!(
                "{STABLE_TOKEN_PREFIX}pidns={}:{raw}",
                current_pid_namespace().unwrap()
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
        recorded_at: now,
        queued: 0,
        deferred: Vec::new(),
        excluded: Vec::new(),
        excluded_total: 0,
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
        recorded_at: now,
        queued: 0,
        deferred: Vec::new(),
        excluded: Vec::new(),
        excluded_total: 0,
        resource_throttle: None,
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
