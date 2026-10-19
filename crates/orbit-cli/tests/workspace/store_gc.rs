//! Store retention through the real CLI: `orbit gc audit`, `orbit gc runs`
//! and `orbit gc journal` plan by default, delete only with `--apply`, and
//! keep what a remaining row still needs.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use orbit_types::workflow::PipelineState;
use rusqlite::{Connection, params};
use serde_json::Value;

const CHILD: &str = "ORBIT_STORE_GC_TEST_CHILD";
const OLD: &str = "2020-01-01T00:00:00+00:00";
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    work: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        fs::create_dir_all(&home).unwrap();
        crate::git_repo::init(&work);
        let fixture = Self {
            _temp: temp,
            home,
            work,
        };
        fixture
            .orbit()
            .args(["workspace", "init", "--name", "retention"])
            .assert()
            .success();
        // Open, and so create, the host store before seeding it.
        fixture
            .orbit()
            .args(["run", "history", "--no-reconcile", "--json"])
            .assert()
            .success();
        fixture
    }

    fn orbit(&self) -> assert_cmd::Command {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&self.work)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("RUST_LOG", "off");
        command.timeout(Duration::from_secs(60));
        command
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = self
            .orbit()
            .env("ORBIT_OPERATOR", "1")
            .args(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice(&output).unwrap()
    }

    fn db(&self) -> Connection {
        Connection::open(self.home.join(".orbit/orbit.db")).unwrap()
    }

    fn count(&self, sql: &str) -> i64 {
        self.db().query_row(sql, [], |row| row.get(0)).unwrap()
    }

    fn workspace_id(&self) -> String {
        let config: Value =
            serde_yaml::from_slice(&fs::read(self.work.join(".orbit/config.yaml")).unwrap())
                .unwrap();
        config["workspace_id"].as_str().unwrap().to_owned()
    }

    fn audit_root(&self) -> PathBuf {
        self.work.join(".orbit/state/audit")
    }

    /// Store a blob named `hash`, last written `age` ago.
    fn blob(&self, hash: &str, age: Duration) -> PathBuf {
        let dir = self.audit_root().join("blobs").join(&hash[..2]);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(hash);
        fs::write(&path, hash.as_bytes()).unwrap();
        set_age(&path, age);
        path
    }
}

fn set_age(path: &Path, age: Duration) {
    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::now() - age)
        .unwrap();
}

fn hash(fill: char) -> String {
    fill.to_string().repeat(64)
}

/// Seeding writes the host store, so the fixture runs only in a child whose
/// inherited Orbit authority is cleared.
fn in_child(test: &str) -> bool {
    if std::env::var(CHILD).as_deref() == Ok(test) {
        return true;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    test_env::clear_inherited_authority(|name| {
        child.env_remove(name);
    });
    child
        .env(CHILD, test)
        .args(["--exact", test, "--nocapture"]);
    let output_dir = tempfile::tempdir().unwrap();
    let output = test_env::run_child_test(&mut child, test, output_dir.path());
    test_env::assert_child_test_passed(test, output.status, output.stdout, output.stderr);
    false
}

fn audit_retention_plans_then_prunes_rows_and_unreferenced_blobs() {
    let fixture = Fixture::new();
    // Hosted CI has no provider CLI, so init leaves its default crew disabled.
    // Retention is deterministic and must run without an enabled provider.
    fs::write(
        fixture.work.join(".orbit/config.toml"),
        "[workflow]\ndefault_crew = \"parked\"\n\n[crews.parked]\nenabled = false\n\
         provider = \"claude\"\nmodel = \"test-model\"\n",
    )
    .unwrap();
    let workspace_id = fixture.workspace_id();
    let now = chrono::Utc::now().to_rfc3339();
    let (dropped, kept, step, orphan, pending) =
        (hash('a'), hash('b'), hash('c'), hash('d'), hash('e'));
    {
        let db = fixture.db();
        for (execution_id, at) in [("old-1", OLD), ("old-2", OLD), ("new", now.as_str())] {
            db.execute(
                "INSERT INTO audit_events (execution_id, timestamp, command, role, status, \
                     exit_code, duration_ms, working_directory, pid) \
                 VALUES (?1, ?2, 'task', 'codex', 'success', 0, 1, '.', 1)",
                params![execution_id, at],
            )
            .unwrap();
        }
        for (event_id, at, blob) in [("evt-old", OLD, &dropped), ("evt-new", now.as_str(), &kept)] {
            db.execute(
                "INSERT INTO v2_audit_events (workspace_id, event_id, source, schema_version, \
                     event_type, ts, run_id, agent_identity, payload_json) \
                 VALUES (?1, ?2, 'loop_event', 1, 'tool.call.result', ?3, 'jrun-x', 'codex', ?4)",
                params![
                    workspace_id,
                    event_id,
                    at,
                    format!("{{\"output_ref\":\"{blob}\"}}")
                ],
            )
            .unwrap();
        }
        db.execute(
            "INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state, scheduled_at, \
                 created_at) VALUES ('jrun-step', ?1, 'fixture', 1, 'success', ?2, ?2)",
            params![workspace_id, now],
        )
        .unwrap();
        db.execute(
            "INSERT INTO job_run_steps (workspace_id, run_id, step_index, target_type, \
                 target_id, state, agent_response_json) \
             VALUES (?1, 'jrun-step', 0, 'activity', 'implement', 'success', ?2)",
            params![workspace_id, format!("{{\"stdout_ref\":\"{step}\"}}")],
        )
        .unwrap();
    }
    let old = 400 * DAY;
    let paths: Vec<PathBuf> = [&dropped, &kept, &step, &orphan, &pending]
        .into_iter()
        .map(|hash| fixture.blob(hash, old))
        .collect();
    // A write in flight: stored and marked, not yet named by any row.
    let marker = fixture.audit_root().join("pending").join(&pending);
    fs::create_dir_all(marker.parent().unwrap()).unwrap();
    fs::write(&marker, b"").unwrap();
    // A blob written moments ago that nothing names yet.
    let recent = fixture.blob(&hash('f'), Duration::ZERO);

    let plan = fixture.json(&["gc", "audit", "--json"]);
    assert_eq!(plan["apply"], false);
    assert_eq!(plan["retention_days"], 60);
    let tables = plan["tables"].as_array().unwrap();
    assert_eq!(tables[0]["table"], "audit_events");
    assert_eq!(tables[0]["rows"], 2);
    assert_eq!(tables[1]["table"], "v2_audit_events");
    assert_eq!(tables[1]["rows"], 1);
    assert_eq!(plan["blobs"]["blobs"], 6);
    assert_eq!(plan["blobs"]["pending"], 1);
    assert_eq!(plan["blobs"]["recent"], 1);
    assert_eq!(
        plan["blobs"]["unreferenced"], 2,
        "the pruned row's blob and the orphan"
    );
    assert_eq!(plan["blobs"]["removed"], 0);
    assert_eq!(
        fixture.count("SELECT COUNT(*) FROM v2_audit_events"),
        2,
        "a plan deletes nothing"
    );
    assert!(paths.iter().all(|path| path.is_file()));

    fixture
        .orbit()
        .args(["gc", "audit", "--apply", "--json"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("capability_denied"));
    let applied = fixture.json(&["gc", "audit", "--apply", "--json"]);
    assert_eq!(applied["apply"], true);
    assert_eq!(applied["tables"][0]["rows_removed"], 2);
    assert_eq!(applied["tables"][1]["rows_removed"], 1);
    assert_eq!(applied["blobs"]["removed"], 2);
    assert!(applied["writes"]["batches"].as_u64().unwrap() >= 2);
    assert!(applied["store"]["file_bytes"].as_u64().unwrap() > 0);

    assert_eq!(
        fixture.count("SELECT COUNT(*) FROM audit_events WHERE execution_id LIKE 'old-%'"),
        0
    );
    assert_eq!(
        fixture.count("SELECT COUNT(*) FROM audit_events WHERE execution_id = 'new'"),
        1
    );
    assert_eq!(
        fixture.count("SELECT COUNT(*) FROM v2_audit_events WHERE event_id = 'evt-new'"),
        1
    );
    assert_eq!(fixture.count("SELECT COUNT(*) FROM v2_audit_events"), 1);
    let [
        dropped_path,
        kept_path,
        step_path,
        orphan_path,
        pending_path,
    ] = paths.as_slice()
    else {
        unreachable!()
    };
    assert!(!dropped_path.exists(), "named only by a pruned row");
    assert!(!orphan_path.exists(), "named by nothing");
    assert!(kept_path.is_file(), "named by a kept audit row");
    assert!(step_path.is_file(), "named by a step response");
    assert!(pending_path.is_file(), "written but not yet published");
    assert!(recent.is_file(), "inside the grace window");
    assert!(marker.is_file());

    let again = fixture.json(&["gc", "audit", "--apply", "--json"]);
    assert_eq!(again["tables"][1]["rows_removed"], 0);
    assert_eq!(again["blobs"]["removed"], 0);
    fixture
        .orbit()
        .args(["gc", "audit", "--older-than-days", "0"])
        .assert()
        .failure();

    // The routine's shipped job applies both sweeps.
    let output = fixture
        .orbit()
        .env("ORBIT_OPERATOR", "1")
        .args(["job", "run", "store_gc_pipeline", "--wait", "--json"])
        .output()
        .unwrap();
    let completed: Value = serde_json::from_slice(&output.stdout).unwrap();
    let run_id = completed["run_id"].as_str().unwrap();
    let shown = fixture.json(&["run", "show", run_id, "--no-reconcile", "--json"]);
    assert!(
        output.status.success(),
        "store retention job failed: {completed}; run: {shown}; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(completed["state"], "success", "{shown}");
    assert!(shown["run"]["resolved_crew"].is_null(), "{shown}");
    let sweep = &shown["pipeline_state"]["pipeline"]["sweep"];
    assert_eq!(sweep["audit"]["apply"], true, "{sweep}");
    assert_eq!(sweep["runs"]["apply"], true, "{sweep}");
    assert!(kept_path.is_file() && step_path.is_file() && pending_path.is_file());

    // Deterministic jobs still refuse an unknown configured crew.
    fs::write(
        fixture.work.join(".orbit/config.toml"),
        "[workflow]\ndefault_crew = \"missing\"\n",
    )
    .unwrap();
    fixture
        .orbit()
        .env("ORBIT_OPERATOR", "1")
        .args(["job", "run", "store_gc_pipeline", "--wait", "--json"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("crew 'missing' is not defined"));
}

fn run_retention_drops_terminal_state_and_keeps_run_history() {
    let fixture = Fixture::new();
    let workspace_id = fixture.workspace_id();
    {
        let db = fixture.db();
        for (run_id, state, finished) in [
            ("jrun-old-done", "success", Some(OLD)),
            ("jrun-old-held", "held", Some(OLD)),
            ("jrun-old-live", "running", None),
        ] {
            db.execute(
                "INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state, \
                     scheduled_at, started_at, finished_at, duration_ms, created_at, pid) \
                 VALUES (?1, ?2, 'fixture', 1, ?3, ?4, ?4, ?5, 1, ?4, ?6)",
                // This test process owns the live run, so reconciliation
                // leaves it running.
                params![
                    run_id,
                    workspace_id,
                    state,
                    OLD,
                    finished,
                    std::process::id()
                ],
            )
            .unwrap();
            db.execute(
                "INSERT INTO job_run_steps (workspace_id, run_id, step_index, target_type, \
                     target_id, state, started_at, finished_at, agent_response_json) \
                 VALUES (?1, ?2, 0, 'activity', 'implement', 'success', ?3, ?3, '{}')",
                params![workspace_id, run_id, OLD],
            )
            .unwrap();
            let state_json = serde_json::to_string(&PipelineState::new(
                run_id.to_string(),
                "fixture".to_string(),
                serde_json::json!({ "padding": "x".repeat(2048) }),
            ))
            .unwrap();
            db.execute(
                "INSERT INTO job_run_states (workspace_id, run_id, pipeline_state_json) \
                 VALUES (?1, ?2, ?3)",
                params![workspace_id, run_id, state_json],
            )
            .unwrap();
        }
    }

    let plan = fixture.json(&["gc", "runs", "--json"]);
    assert_eq!(plan["apply"], false);
    assert_eq!(plan["runs"], 1, "only the old finished run");
    assert!(plan["state_bytes"].as_u64().unwrap() >= 2048);
    assert_eq!(fixture.count("SELECT COUNT(*) FROM job_run_states"), 3);

    let applied = fixture.json(&["gc", "runs", "--apply", "--json"]);
    assert_eq!(applied["runs_archived"], 1);
    let remaining: Vec<String> = fixture
        .db()
        .prepare("SELECT run_id FROM job_run_states ORDER BY run_id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(remaining, ["jrun-old-held", "jrun-old-live"]);
    assert_eq!(
        fixture.count(
            "SELECT COUNT(*) FROM job_runs WHERE run_id = 'jrun-old-done' \
             AND archived_at IS NOT NULL"
        ),
        1
    );
    assert_eq!(
        fixture.count("SELECT COUNT(*) FROM job_run_steps WHERE run_id LIKE 'jrun-old-%'"),
        3
    );
    assert_eq!(
        fixture.count(
            "SELECT COUNT(*) FROM job_runs WHERE run_id = 'jrun-old-live' AND state = 'running'"
        ),
        1,
        "a live run is never touched"
    );

    let shown = fixture.json(&["run", "show", "jrun-old-done", "--json"]);
    assert_eq!(shown["run"]["state"], "success");
    assert_eq!(shown["run"]["steps"].as_array().unwrap().len(), 1);
    assert!(
        shown["pipeline_state"].is_null(),
        "its pipeline state is gone"
    );
    let history = fixture.json(&["run", "history", "--no-reconcile", "--json"]);
    assert!(
        history.to_string().contains("jrun-old-done"),
        "an archived run stays in run history"
    );

    let doctor = fixture.orbit().args(["doctor", "--json"]).output().unwrap();
    let doctor: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    let check = find_check(&doctor, "store-retention").expect("store-retention doctor check");
    assert_eq!(check["status"], "ok");
}

/// The per-minute run and host resource history, and the admission forecasts
/// checked against it, are pruned by run retention after 30 days, host-wide,
/// whatever window the run sweep uses.
fn run_retention_prunes_resource_minutes_past_thirty_days() {
    let fixture = Fixture::new();
    let stamp = |days: i64| {
        (chrono::Utc::now() - chrono::Duration::days(days))
            .to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
    };
    {
        let db = fixture.db();
        for (workspace, run_id, days) in [
            ("ws_a", "jrun-old", 31),
            ("ws_b", "jrun-other-workspace", 45),
            ("ws_a", "jrun-recent", 29),
        ] {
            db.execute(
                "INSERT INTO run_resource_minutes (workspace_id, run_id, minute_index, \
                     minute_started_at, task_id, work_class, complexity, resumed, \
                     delivery_admitted_at, cpu_cores, memory_bytes, samples) \
                 VALUES (?1, ?2, 0, ?3, 'ORB-1', 'change', 'hard', 0, ?3, 1.5, 1024, 12)",
                params![workspace, run_id, stamp(days)],
            )
            .unwrap();
        }
        for days in [40, 31, 1] {
            db.execute(
                "INSERT INTO host_resource_minutes (minute_at, cpu_cores, cpu_count, \
                     memory_bytes, memory_total_bytes, samples, recorded_by_run_id) \
                 VALUES (?1, 3.0, 16, 1024, 4096, 12, 'jrun-old')",
                params![stamp(days)],
            )
            .unwrap();
        }
        for (workspace, days) in [("ws_a", 31), ("ws_b", 60), ("ws_a", 2)] {
            db.execute(
                "INSERT INTO admission_forecasts (workspace_id, run_id, forecast_at, mode, \
                     cpu_count, memory_total_bytes, points_json, admitted_json, \
                     fit_admitted_json, fit_skipped_json) \
                 VALUES (?1, 'jrun-drain', ?2, 'shadow', 16, 4096, '[]', '[]', '[]', '[]')",
                params![workspace, stamp(days)],
            )
            .unwrap();
        }
    }

    let plan = fixture.json(&["gc", "runs", "--json"]);
    let tables = plan["resource_minutes"].as_array().unwrap();
    assert_eq!(tables[0]["table"], "run_resource_minutes");
    assert_eq!(tables[0]["retention_days"], 30);
    assert_eq!(tables[0]["rows"], 2);
    assert_eq!(tables[1]["table"], "host_resource_minutes");
    assert_eq!(tables[1]["rows"], 2);
    assert_eq!(tables[2]["table"], "admission_forecasts");
    assert_eq!(tables[2]["retention_days"], 30);
    assert_eq!(tables[2]["rows"], 2);
    assert_eq!(
        fixture.count("SELECT COUNT(*) FROM run_resource_minutes"),
        3
    );

    let applied = fixture.json(&[
        "gc",
        "runs",
        "--apply",
        "--older-than-days",
        "365",
        "--json",
    ]);
    assert_eq!(applied["resource_minutes"][0]["rows_removed"], 2);
    assert_eq!(applied["resource_minutes"][1]["rows_removed"], 2);
    assert_eq!(applied["resource_minutes"][2]["rows_removed"], 2);
    assert_eq!(
        fixture.count("SELECT COUNT(*) FROM run_resource_minutes WHERE run_id = 'jrun-recent'"),
        1
    );
    assert_eq!(
        fixture.count("SELECT COUNT(*) FROM run_resource_minutes"),
        1
    );
    assert_eq!(
        fixture.count("SELECT COUNT(*) FROM host_resource_minutes"),
        1
    );
    assert_eq!(fixture.count("SELECT COUNT(*) FROM admission_forecasts"), 1);
}

/// `orbit gc journal` plans by default and deletes applied commit journal
/// rows past `retention.journal_days` only with operator `--apply`. It keeps
/// each task's newest applied row and every row that is not applied, and its
/// report names counts, never journal or task ids.
fn journal_retention_plans_then_prunes_applied_rows() {
    let fixture = Fixture::new();
    let workspace_id = fixture.workspace_id();
    let days_ago = |days: i64| (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();
    {
        let db = fixture.db();
        for (journal_id, task_id, state, age_days) in [
            ("commit-old-1", "ORB-90001", "applied", 40),
            ("commit-old-2", "ORB-90001", "applied", 30),
            ("commit-old-3", "ORB-90001", "applied", 20),
            ("commit-fresh", "ORB-90001", "applied", 1),
            ("commit-lone", "ORB-90002", "applied", 40),
            ("commit-aborted", "ORB-90002", "aborted", 50),
        ] {
            let at = days_ago(age_days);
            db.execute(
                "INSERT INTO task_commit_journal (journal_id, workspace_id, task_id, state, \
                     intent_json, created_at, committed_at, applied_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, CASE WHEN ?4 = 'applied' THEN ?6 END)",
                params![
                    journal_id,
                    workspace_id,
                    task_id,
                    state,
                    format!("{{\"pad\":\"{}\"}}", "x".repeat(1024)),
                    at
                ],
            )
            .unwrap();
        }
    }

    let plan = fixture.json(&["gc", "journal", "--json"]);
    assert_eq!(plan["apply"], false);
    assert_eq!(plan["retention_days"], 14);
    assert_eq!(plan["rows"], 3, "the old rows of a task with a newer one");
    assert!(plan["bytes"].as_u64().unwrap() >= 3 * 1024);
    assert_eq!(plan["rows_removed"], 0);
    assert_eq!(
        fixture.count("SELECT COUNT(*) FROM task_commit_journal"),
        6,
        "a plan deletes nothing"
    );
    let narrowed = fixture.json(&["gc", "journal", "--older-than-days", "35", "--json"]);
    assert_eq!(
        narrowed["rows"], 1,
        "--older-than-days overrides the window"
    );
    let text = fixture
        .orbit()
        .args(["gc", "journal"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(text).unwrap();
    assert!(
        text.contains("rows=3") && !text.contains("commit-") && !text.contains("ORB-9000"),
        "the report names counts, not ids: {text}"
    );

    fixture
        .orbit()
        .args(["gc", "journal", "--apply", "--json"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("capability_denied"));
    let applied = fixture.json(&["gc", "journal", "--apply", "--json"]);
    assert_eq!(applied["apply"], true);
    assert_eq!(applied["rows_removed"], 3);
    assert_eq!(applied["writes"]["batches"], 1);
    let remaining: Vec<String> = fixture
        .db()
        .prepare("SELECT journal_id FROM task_commit_journal ORDER BY journal_id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(remaining, ["commit-aborted", "commit-fresh", "commit-lone"]);

    let again = fixture.json(&["gc", "journal", "--apply", "--json"]);
    assert_eq!(again["rows"], 0);
    assert_eq!(again["rows_removed"], 0);
    fixture
        .orbit()
        .args(["gc", "journal", "--older-than-days", "0"])
        .assert()
        .failure();
}

fn doctor_check(fixture: &Fixture, name: &str) -> Value {
    let doctor = fixture.orbit().args(["doctor", "--json"]).output().unwrap();
    assert!(
        doctor.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&doctor.stderr)
    );
    let doctor: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    find_check(&doctor, name)
        .unwrap_or_else(|| panic!("missing {name} in {doctor}"))
        .clone()
}

fn set_store_warn_bytes(fixture: &Fixture, bytes: u64) {
    let path = fixture.work.join(".orbit/config.toml");
    let mut text = fs::read_to_string(&path).unwrap_or_default();
    const MARKER: &str = "\n# store-warn-bytes-test\n";
    if let Some(start) = text.find(MARKER) {
        text.truncate(start);
    }
    text.push_str(&format!(
        "{MARKER}[retention]\nstore_warn_bytes = {bytes}\n"
    ));
    fs::write(&path, text).unwrap();
}

/// A store past `retention.store_warn_bytes` warns, and names how to enable
/// store GC, while GC is disabled. Under the threshold, or once the routine
/// is enabled, the same check is ok.
fn store_retention_warns_only_when_the_store_is_large_and_gc_is_disabled() {
    let fixture = Fixture::new();
    assert_eq!(
        doctor_check(&fixture, "store-retention")["status"],
        "ok",
        "the default threshold leaves a fixture store ok"
    );

    set_store_warn_bytes(&fixture, 1);
    let warning = doctor_check(&fixture, "store-retention");
    assert_eq!(warning["status"], "warning", "{warning}");
    let remediation = warning["remediation"].as_str().expect("remediation");
    assert!(
        remediation.contains("orbit.routine.control")
            && remediation.contains("enabled")
            && remediation.contains(".orbit/routines/store_gc.yaml")
            && remediation.contains("job:store_gc_pipeline"),
        "the warning names the enable command: {remediation}"
    );

    set_store_warn_bytes(&fixture, 10 * 1024 * 1024 * 1024);
    assert_eq!(
        doctor_check(&fixture, "store-retention")["status"],
        "ok",
        "under the configured threshold the check is ok"
    );

    set_store_warn_bytes(&fixture, 1);
    // `workspace init` does not seed routines; the check reads whatever YAML
    // is under `.orbit/routines`. A disabled definition still warns.
    let routines = fixture.work.join(".orbit/routines");
    fs::create_dir_all(&routines).unwrap();
    let routine = routines.join("store_gc.yaml");
    fs::write(
        &routine,
        "schemaVersion: 1\nname: store-gc-retention\nenabled: false\ntrigger:\n  \
         cron: \"17 4 * * *\"\n  missed_run: skip\ntarget: job:store_gc_pipeline\n",
    )
    .unwrap();
    assert_eq!(
        doctor_check(&fixture, "store-retention")["status"],
        "warning",
        "a disabled store-gc routine still warns"
    );
    let raw = fs::read_to_string(&routine).unwrap();
    fs::write(&routine, raw.replacen("enabled: false", "enabled: true", 1)).unwrap();
    assert_eq!(
        doctor_check(&fixture, "store-retention")["status"],
        "ok",
        "an enabled store-gc routine keeps the check ok"
    );
}

fn workspace_id_at(work: &Path) -> String {
    let config: Value =
        serde_yaml::from_slice(&fs::read(work.join(".orbit/config.yaml")).unwrap()).unwrap();
    config["workspace_id"].as_str().unwrap().to_owned()
}

/// Plan (the default, which is the dry run) counts an unregistered workspace's
/// old rows. `--apply` removes them. A second registered workspace's old rows,
/// and rows still inside the window, stay.
fn legacy_workspace_rows_are_counted_and_removed_without_touching_registered_ones() {
    let fixture = Fixture::new();
    let current = fixture.workspace_id();
    let other_work = fixture._temp.path().join("kept");
    crate::git_repo::init(&other_work);
    fixture
        .orbit()
        .current_dir(&other_work)
        .args(["workspace", "init", "--name", "retention-kept"])
        .assert()
        .success();
    let registered = workspace_id_at(&other_work);
    assert_ne!(current, registered);
    let legacy = "legacy-unregistered";
    let now = chrono::Utc::now().to_rfc3339();
    {
        let db = fixture.db();
        for (workspace_id, event_id, at) in [
            (current.as_str(), "current-old", OLD),
            (current.as_str(), "current-new", now.as_str()),
            (registered.as_str(), "registered-old", OLD),
            (legacy, "legacy-old", OLD),
            (legacy, "legacy-new", now.as_str()),
        ] {
            db.execute(
                "INSERT INTO v2_audit_events (workspace_id, event_id, source, schema_version, \
                     event_type, ts, run_id, agent_identity, payload_json) \
                 VALUES (?1, ?2, 'loop_event', 1, 'tool.call.result', ?3, ?2, 'codex', '{}')",
                params![workspace_id, event_id, at],
            )
            .unwrap();
        }
        for (workspace_id, run_id, state, finished) in [
            (current.as_str(), "current-old", "success", Some(OLD)),
            (
                current.as_str(),
                "current-new",
                "success",
                Some(now.as_str()),
            ),
            (current.as_str(), "current-held", "held", Some(OLD)),
            (registered.as_str(), "registered-old", "success", Some(OLD)),
            (legacy, "legacy-old", "success", Some(OLD)),
            (legacy, "legacy-new", "success", Some(now.as_str())),
            (legacy, "legacy-held", "held", Some(OLD)),
        ] {
            db.execute(
                "INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state, \
                     scheduled_at, started_at, finished_at, duration_ms, created_at, pid) \
                 VALUES (?1, ?2, 'fixture', 1, ?3, ?4, ?4, ?5, 1, ?4, ?6)",
                params![
                    run_id,
                    workspace_id,
                    state,
                    OLD,
                    finished,
                    std::process::id()
                ],
            )
            .unwrap();
            db.execute(
                "INSERT INTO job_run_states (workspace_id, run_id, pipeline_state_json) \
                 VALUES (?1, ?2, '{}')",
                params![workspace_id, run_id],
            )
            .unwrap();
        }
    }

    let audit_rows = |report: &Value| {
        report["tables"]
            .as_array()
            .unwrap()
            .iter()
            .find(|table| table["table"] == "v2_audit_events")
            .unwrap()["rows"]
            .as_u64()
            .unwrap()
    };
    let before = fixture.count("SELECT COUNT(*) FROM v2_audit_events");
    let plan = fixture.json(&["gc", "audit", "--json"]);
    assert_eq!(plan["apply"], false);
    assert_eq!(
        audit_rows(&plan),
        2,
        "current and legacy rows past the cutoff, not the other workspace: {plan}"
    );
    assert_eq!(
        fixture.count("SELECT COUNT(*) FROM v2_audit_events"),
        before,
        "a plan deletes nothing"
    );

    let applied = fixture.json(&["gc", "audit", "--apply", "--json"]);
    assert_eq!(applied["tables"][1]["rows_removed"], 2, "{applied}");
    assert_eq!(
        fixture.count(
            "SELECT COUNT(*) FROM v2_audit_events WHERE event_id IN ('current-old', 'legacy-old')"
        ),
        0
    );
    assert_eq!(
        fixture.count(
            "SELECT COUNT(*) FROM v2_audit_events \
             WHERE event_id IN ('current-new', 'registered-old', 'legacy-new')"
        ),
        3,
        "registered rows and rows inside the window stay"
    );

    let runs_before = fixture.count("SELECT COUNT(*) FROM job_run_states");
    let runs = fixture.json(&["gc", "runs", "--json"]);
    assert_eq!(runs["apply"], false);
    assert_eq!(
        runs["runs"], 2,
        "only the current and legacy finished runs past the cutoff: {runs}"
    );
    assert_eq!(
        fixture.count("SELECT COUNT(*) FROM job_run_states"),
        runs_before
    );

    let archived = fixture.json(&["gc", "runs", "--apply", "--json"]);
    assert_eq!(archived["runs_archived"], 2, "{archived}");
    let remaining: Vec<String> = fixture
        .db()
        .prepare("SELECT run_id FROM job_run_states ORDER BY run_id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        remaining,
        [
            "current-held",
            "current-new",
            "legacy-held",
            "legacy-new",
            "registered-old",
        ]
    );
}

fn find_check<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    match value {
        Value::Object(map) => {
            if map.get("check").and_then(Value::as_str) == Some(name) {
                return Some(value);
            }
            map.values().find_map(|value| find_check(value, name))
        }
        Value::Array(items) => items.iter().find_map(|value| find_check(value, name)),
        _ => None,
    }
}

/// Retention is deterministic, so it runs on a host whose default crew is
/// disabled (a host with no provider CLI seeds every crew disabled) and
/// records no crew for the run. A crew lookup still guards misconfiguration.
#[test]
fn store_retention_contracts() {
    if !in_child("store_gc::store_retention_contracts") {
        return;
    }
    for (name, check) in [
        (
            "audit_retention_plans_then_prunes_rows_and_unreferenced_blobs",
            audit_retention_plans_then_prunes_rows_and_unreferenced_blobs as fn(),
        ),
        (
            "run_retention_drops_terminal_state_and_keeps_run_history",
            run_retention_drops_terminal_state_and_keeps_run_history as fn(),
        ),
        (
            "run_retention_prunes_resource_minutes_past_thirty_days",
            run_retention_prunes_resource_minutes_past_thirty_days as fn(),
        ),
        (
            "journal_retention_plans_then_prunes_applied_rows",
            journal_retention_plans_then_prunes_applied_rows as fn(),
        ),
        (
            "store_retention_warns_only_when_the_store_is_large_and_gc_is_disabled",
            store_retention_warns_only_when_the_store_is_large_and_gc_is_disabled as fn(),
        ),
        (
            "legacy_workspace_rows_are_counted_and_removed_without_touching_registered_ones",
            legacy_workspace_rows_are_counted_and_removed_without_touching_registered_ones
                as fn(),
        ),
    ] {
        tracing::info!(case = name, "CLI contract row");
        check();
        orbit_common::test_env::record_scenario_pass(name);
    }
}

#[test]
fn run_retention_refuses_windows_shorter_than_analytics_history() {
    const TEST: &str = "store_gc::run_retention_refuses_windows_shorter_than_analytics_history";
    if !in_child(TEST) {
        return;
    }
    let fixture = Fixture::new();
    let config_path = fixture.work.join(".orbit/config.toml");
    fs::write(&config_path, "").unwrap();
    for value in ["0", "14", "29", "0x0e"] {
        fixture
            .orbit()
            .env("ORBIT_OPERATOR", "1")
            .args(["config", "set", "retention.runs_days", value])
            .assert()
            .failure()
            .stderr(predicates::str::contains("require 30 days"));
        assert_eq!(fs::read_to_string(&config_path).unwrap(), "");
        fixture
            .orbit()
            .env("ORBIT_OPERATOR", "1")
            .args(["gc", "runs", "--older-than-days", value, "--apply"])
            .assert()
            .failure();
    }
    fixture
        .orbit()
        .env("ORBIT_OPERATOR", "1")
        .args(["config", "set", "retention.runs_days", "30"])
        .assert()
        .success();
    assert_eq!(
        fixture.json(&["gc", "runs", "--json"])["retention_days"],
        30
    );
}

/// Previously recommended persisted windows must not prevent startup or repair.
#[test]
fn stored_short_run_retention_loads_safely_and_can_be_repaired() {
    const TEST: &str = "store_gc::stored_short_run_retention_loads_safely_and_can_be_repaired";
    if !in_child(TEST) {
        return;
    }
    for global in [false, true] {
        let fixture = Fixture::new();
        let config_path = if global {
            fixture.home.join(".orbit/config.toml")
        } else {
            fixture.work.join(".orbit/config.toml")
        };
        let mut stored = fs::read_to_string(&config_path).unwrap_or_default();
        stored.push_str("\n[retention]\nruns_days = 14\n");
        fs::write(&config_path, &stored).unwrap();

        let output = fixture
            .orbit()
            .env("ORBIT_OPERATOR", "1")
            .env("RUST_LOG", "warn")
            .args(["gc", "runs", "--json"])
            .assert()
            .success()
            .stderr(predicates::str::contains("retention.runs_days"))
            .get_output()
            .stdout
            .clone();
        let plan: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(plan["retention_days"], 30);
        assert_eq!(fs::read_to_string(&config_path).unwrap(), stored);

        let mut repair = fixture.orbit();
        repair.env("ORBIT_OPERATOR", "1").args(["config", "set"]);
        if global {
            repair.arg("--global");
        }
        repair
            .args(["retention.runs_days", "30"])
            .assert()
            .success();
        let repaired = fs::read_to_string(&config_path).unwrap();
        let document: toml::Value = toml::from_str(&repaired).unwrap();
        assert_eq!(document["retention"]["runs_days"].as_integer(), Some(30));

        let mut refused = fixture.orbit();
        refused.env("ORBIT_OPERATOR", "1").args(["config", "set"]);
        if global {
            refused.arg("--global");
        }
        refused
            .args(["retention.runs_days", "14"])
            .assert()
            .failure();
        assert_eq!(fs::read_to_string(&config_path).unwrap(), repaired);
        assert_eq!(
            fixture.json(&["gc", "runs", "--json"])["retention_days"],
            30
        );
    }
}
