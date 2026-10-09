//! Store retention through the real CLI: `orbit gc audit` and `orbit gc runs`
//! plan by default, delete only with `--apply`, and keep what a remaining row
//! still needs.

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
    let output = child
        .env(CHILD, test)
        .args(["--exact", test, "--nocapture"])
        .output()
        .unwrap();
    test_env::assert_child_test_passed(test, output.status, output.stdout, output.stderr);
    false
}

#[test]
fn audit_retention_plans_then_prunes_rows_and_unreferenced_blobs() {
    const TEST: &str = "store_gc::audit_retention_plans_then_prunes_rows_and_unreferenced_blobs";
    if !in_child(TEST) {
        return;
    }
    let fixture = Fixture::new();
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
    let completed = fixture.json(&["job", "run", "store_gc_pipeline", "--wait", "--json"]);
    assert_eq!(completed["state"], "success", "{completed}");
    let run_id = completed["run_id"].as_str().unwrap();
    let shown = fixture.json(&["run", "show", run_id, "--no-reconcile", "--json"]);
    let sweep = &shown["pipeline_state"]["pipeline"]["sweep"];
    assert_eq!(sweep["audit"]["apply"], true, "{sweep}");
    assert_eq!(sweep["runs"]["apply"], true, "{sweep}");
    assert!(kept_path.is_file() && step_path.is_file() && pending_path.is_file());
}

#[test]
fn run_retention_drops_terminal_state_and_keeps_run_history() {
    const TEST: &str = "store_gc::run_retention_drops_terminal_state_and_keeps_run_history";
    if !in_child(TEST) {
        return;
    }
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
fn retention_job_runs_when_the_default_crew_is_disabled() {
    let fixture = Fixture::new();
    fs::write(
        fixture.work.join(".orbit/config.toml"),
        "[workflow]\ndefault_crew = \"parked\"\n\n[crews.parked]\nenabled = false\n\
         provider = \"claude\"\nmodel = \"test-model\"\n",
    )
    .unwrap();

    let completed = fixture.json(&["job", "run", "store_gc_pipeline", "--wait", "--json"]);
    assert_eq!(completed["state"], "success", "{completed}");
    let run_id = completed["run_id"].as_str().unwrap();
    let shown = fixture.json(&["run", "show", run_id, "--no-reconcile", "--json"]);
    assert!(
        shown["run"].get("resolved_crew").is_none_or(Value::is_null),
        "{shown}"
    );

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
        .stdout(predicates::str::contains("missing"));
}
