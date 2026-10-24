//! `--no-reconcile` run reads leave stale runs and reservations untouched;
//! explicit lookups keep store errors distinct, and live cancellation is driven end to end.
#![allow(missing_docs)]
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
use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use orbit_core::OrbitRuntime;
use orbit_types::workflow::{DrainAdmissionPass, PipelineState};
use rusqlite::{Connection, params};
use serde_json::Value;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
mod admission_forecast;
mod auth_exclusions;
mod build_budget;
mod capacity_backoff;
mod drain_window;
#[cfg(target_os = "macos")]
mod host_resources;
#[cfg(unix)]
mod job_exit_status;
mod provider_limits;
mod queued_drains;
mod repair_skips;
mod run_history;
mod run_show_display;
mod wait;
mod waiting_blockers;
const STALE_RUNNING: &str = "jrun-20260920-0100";
const STALE_PENDING: &str = "jrun-20260920-0200";
const FAILED: &str = "jrun-20260920-0300";
struct CalmHost;
impl orbit_core::runtime::host_resource::HostResourceProbe for CalmHost {
    fn sample(
        &self,
        disk_paths: &[std::path::PathBuf],
    ) -> orbit_core::runtime::host_resource::HostResourceSample {
        orbit_core::runtime::host_resource::HostResourceSample {
            sampled_at: chrono::Utc::now(),
            cpu_percent: Some(10.0),
            cpu_utilization_percent: None,
            memory_percent: Some(10.0),
            io_full_percent: None,
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
fn successful_security_sweep_shows_filing_floor_source_and_excluded_alerts() {
    let fixture = Fixture::init();
    let runtime = fixture.runtime();
    for source in [Some("workspace"), None] {
        let id = format!("jrun-cli-security-{}", source.unwrap_or("historical"));
        let now = chrono::Utc::now().to_rfc3339();
        fixture.db().execute("INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,scheduled_at,started_at,finished_at,created_at) VALUES (?1,?2,'dependabot_alert_sweep_pipeline',1,'success',?3,?3,?3,?3)", params![id, fixture.workspace_id(), now]).unwrap();
        let mut state = fixture.write_state(
            &runtime,
            &id,
            "dependabot_alert_sweep_pipeline",
            serde_json::json!({}),
        );
        let mut output = serde_json::json!({"filed_count": 1, "min_severity": "high", "excluded_below_min_severity": [{"family": "dependabot", "number": 71, "severity": "moderate"}, {"family": "code_scanning", "alert_number": 81, "security_severity": "moderate"}]});
        if let Some(source) = source {
            output["min_severity_source"] = serde_json::json!(source);
        }
        state.record_pipeline_output("file", output);
        runtime.write_run_state(&id, &state).unwrap();
        let text = fixture.show_text(&id);
        contains(&text, "filed=1");
        contains(
            &text,
            &format!("min_severity=high ({})", source.unwrap_or("unavailable")),
        );
        assert!(
            text.contains("2 (dependabot #71, code_scanning #81)"),
            "successful sweeps must expose excluded alerts (2026-10-06/07 incident): {text}"
        );
    }
}
fn shipped_security_job_uses_config_unless_run_input_overrides_it() {
    let fixture = Fixture::init();
    for (config, input, floor, source) in [
        ("", None, "moderate", "built-in"),
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
        command.args(["run", "job", "dependabot_alert_sweep_pipeline", "--json"]);
        if source == "built-in" {
            command.arg("--wait");
        }
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
        if source != "built-in" {
            let runtime =
                orbit_cmd::registry_runtime::RegisteredRuntimeFactory::open_resolved_roots(
                    orbit_core::runtime::OrbitRuntimeRoots {
                        global_root: fixture.home.join(".orbit"),
                        shared_root: fixture.work.join(".orbit"),
                        local_root: fixture.work.join(".orbit"),
                    },
                )
                .expect("registered security fixture runtime");
            let waited = runtime
                .wait_pipeline_runs(
                    &[submitted["run_id"]
                        .as_str()
                        .expect("submitted sweep")
                        .to_string()],
                    30,
                    1,
                    None,
                )
                .expect("wait for the actual CLI-submitted security sweep");
            assert_eq!(waited.results.len(), 1);
            assert_eq!(
                waited.results[0].status,
                orbit_core::PipelineWaitStatus::Success
            );
        }
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
        Self::init_with_stale_runs(true)
    }
    fn terminal() -> Self {
        Self::init_with_stale_runs(false)
    }
    fn init_with_stale_runs(include_stale_runs: bool) -> Self {
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
        fixture_crew::configure_sol(&fixture.home.join(".orbit"));
        drop(
            orbit_cmd::registry_runtime::RegisteredRuntimeFactory::open_resolved_roots(
                orbit_core::runtime::OrbitRuntimeRoots {
                    global_root: fixture.home.join(".orbit"),
                    shared_root: fixture.work.join(".orbit"),
                    local_root: fixture.work.join(".orbit"),
                },
            )
            .expect("registered observation fixture runtime"),
        );
        fixture.seed(include_stale_runs);
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
    fn seed(&self, include_stale_runs: bool) {
        let workspace_id = self.workspace_id();
        let orbit_dir = canonical(&self.work).join(".orbit");
        let orbit_dir = orbit_dir.to_string_lossy();
        let db = self.db();
        if include_stale_runs {
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
                db.execute("INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state, scheduled_at, started_at, created_at, pid) VALUES (?1, ?2, 'fixture_job', 1, ?3, ?4, ?5, ?4, ?6)", params![run_id, workspace_id, state, created_at, started_at, pid]).expect("seed stale run");
                db.execute("INSERT INTO task_reservations (reservation_id, workspace_orbit_dir, workspace_id, task_ids_json, files_json, actor, created_at, expires_at, owner_run_id) VALUES (?1, ?2, ?3, '[]', ?4, 'fixture', ?5, '2099-01-01T00:00:00+00:00', ?6)", params![format!("reservation-{run_id}"), orbit_dir, workspace_id, format!(r#"["src/{state}.rs"]"#), created_at, run_id]).expect("seed reservation");
            }
        }
        db.execute("INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state, scheduled_at, started_at, finished_at, duration_ms, created_at) VALUES (?1, ?2, 'fixture_job', 1, 'failed', '2026-09-20T03:00:00+00:00', '2026-09-20T03:00:01+00:00', '2026-09-20T03:05:00+00:00', 299000, '2026-09-20T03:00:00+00:00')", params![FAILED, workspace_id]).expect("seed failed run");
        db.execute("INSERT INTO job_run_steps (workspace_id, run_id, step_index, target_type, target_id, state, started_at, finished_at, duration_ms, error_code, error_message) VALUES (?1, ?2, 0, 'activity', 'implement', 'failed', '2026-09-20T03:00:01+00:00', '2026-09-20T03:05:00+00:00', 299000, 'fixture_error', 'fixture step failed')", params![workspace_id, FAILED]).expect("seed failed step");
    }
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
    fn runtime(&self) -> OrbitRuntime {
        OrbitRuntime::from_roots(&self.home.join(".orbit"), &self.work.join(".orbit")).unwrap()
    }
    fn text(&self, args: &[&str]) -> String {
        let output = self.orbit().args(args).output().expect("spawn orbit");
        assert!(
            output.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }
    fn show_json(&self, id: &str) -> Value {
        self.json(&["run", "show", id, "--no-reconcile", "--json"])
    }
    fn show_text(&self, id: &str) -> String {
        self.text(&["run", "show", id, "--no-reconcile"])
    }
    fn readiness_text(&self) -> String {
        self.text(&["run", "readiness"])
    }
    fn doctor_row(&self, check: &str) -> Value {
        let out = self.orbit().args(["doctor", "--json"]).output().unwrap();
        let rows: Value = serde_json::from_slice(&out.stdout).unwrap();
        rows.as_array()
            .unwrap()
            .iter()
            .find(|r| r["check"] == check)
            .unwrap_or_else(|| panic!("no doctor row {check}: {rows}"))
            .clone()
    }
    fn write_state(
        &self,
        runtime: &OrbitRuntime,
        id: &str,
        job: &str,
        input: Value,
    ) -> PipelineState {
        let state = PipelineState::new(id.into(), job.into(), input);
        runtime.write_run_state(id, &state).unwrap();
        state
    }
    fn seed_running(&self, id: &str, job: &str, input: Value) -> (OrbitRuntime, PipelineState) {
        let runtime = self.runtime();
        let now = chrono::Utc::now().to_rfc3339();
        self.db().execute("INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,input_json,scheduled_at,started_at,created_at,pid) VALUES (?1,?2,?3,1,'running',?4,?5,?5,?5,?6)", params![id, self.workspace_id(), job, input.to_string(), now, std::process::id()]).unwrap();
        let state = self.write_state(&runtime, id, job, input);
        (runtime, state)
    }
    fn seed_pass(
        &self,
        id: &str,
        job: &str,
        pass: DrainAdmissionPass,
    ) -> (OrbitRuntime, PipelineState) {
        let (runtime, mut state) = self.seed_running(id, job, serde_json::json!({}));
        state.pull_drain.drain_last_pass = Some(pass);
        runtime.write_run_state(id, &state).unwrap();
        (runtime, state)
    }
    fn set_run_state(&self, id: &str, state: &str) {
        self.db()
            .execute(
                "UPDATE job_runs SET state=?1 WHERE run_id=?2",
                params![state, id],
            )
            .unwrap();
    }
    fn conc(&self, id: &str, set: &str, rev: Option<&str>, reason: Option<&str>) -> Value {
        let mut args = vec!["run", "concurrency", id, "--set", set];
        if let Some(rev) = rev {
            args.extend(["--if-revision", rev]);
        }
        if let Some(reason) = reason {
            args.extend(["--reason", reason]);
        }
        args.push("--json");
        self.json(&args)
    }
    fn conc_err(&self, id: &str, set: &str, rev: Option<&str>) -> Value {
        let mut args = vec!["run", "concurrency", id, "--set", set];
        if let Some(rev) = rev {
            args.extend(["--if-revision", rev]);
        }
        args.push("--json");
        self.failure(&args)
    }
    fn extend(&self, id: &str, dur: &str, conc: Option<&str>, reason: Option<&str>) -> Value {
        let mut args = vec!["run", "extend", id, "--for", dur];
        if let Some(conc) = conc {
            args.extend(["--concurrency", conc]);
        }
        if let Some(reason) = reason {
            args.extend(["--reason", reason]);
        }
        args.push("--json");
        self.json(&args)
    }
    fn extend_err(&self, id: &str, dur: &str, conc: Option<&str>) -> Value {
        let mut args = vec!["run", "extend", id, "--for", dur];
        if let Some(conc) = conc {
            args.extend(["--concurrency", conc]);
        }
        args.push("--json");
        self.failure(&args)
    }
    fn config_set(&self, key: &str, value: &str) {
        self.orbit()
            .args(["config", "set", "--seed-from-global", key, value])
            .assert()
            .success();
    }
    fn failure(&self, args: &[&str]) -> Value {
        let output = self.orbit().args(args).output().expect("spawn orbit");
        assert!(!output.status.success(), "{args:?} unexpectedly succeeded");
        let stderr = String::from_utf8_lossy(&output.stderr);
        serde_json::from_str(&stderr)
            .unwrap_or_else(|_| panic!("{args:?} stderr is not one JSON error: {stderr}"))
    }
}
fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
fn pass_at(recorded_at: chrono::DateTime<chrono::Utc>) -> DrainAdmissionPass {
    serde_json::from_value(serde_json::json!({"recorded_at": recorded_at, "queued": 0})).unwrap()
}
fn waiting(
    task: &str,
    reason: &str,
    blocked_by: &[&str],
    detail: Option<&str>,
) -> orbit_types::workflow::DrainWaitingTask {
    serde_json::from_value(serde_json::json!({"task_id": task, "reason": reason, "blocked_by": blocked_by, "detail": detail})).unwrap()
}
fn dispatch(
    state: &mut PipelineState,
    child: &str,
    job: &str,
    step: &str,
    now: chrono::DateTime<chrono::Utc>,
) {
    state.record_child_dispatch(orbit_types::workflow::ChildDispatch::submitted(
        child.into(),
        job.into(),
        step.into(),
        false,
        false,
        now,
    ));
}
fn run_det(runtime: &OrbitRuntime, name: &str, input: Value) -> Value {
    orbit_engine::RuntimeHost::run_deterministic(
        runtime,
        name,
        &serde_json::json!({}),
        &input,
        orbit_tools::ToolContext::default(),
    )
    .unwrap()
}
fn task_params(
    title: String,
    description: String,
    status: orbit_core::TaskStatus,
) -> orbit_core::application::task::TaskAddParams {
    orbit_core::application::task::TaskAddParams {
        title,
        description,
        task_type: Some(orbit_core::TaskType::Feature),
        complexity: orbit_core::TaskComplexity::Low,
        status: Some(status),
        ..Default::default()
    }
}
fn cap(report: &Value) -> &Value {
    &report["capacity"]
}
fn contains(text: &str, needle: &str) {
    assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
}
macro_rules! contract {
    ($name:ident, $($f:ident),+ $(,)?) => {
        #[test]
        fn $name() {
            if !isolated_run_observation(concat!("run_observation::", stringify!($name))) { return; }
            run_rows(&[$((stringify!($f), $f as fn())),+]);
        }
    };
}
fn run_rows(rows: &[(&str, fn())]) {
    for (name, check) in rows {
        tracing::info!(case = name, "CLI contract row");
        check();
        orbit_common::test_env::record_scenario_pass(name);
    }
}
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
    assert_eq!(cap(&report)["max_active_leaf_runs"], 3);
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
fn readiness_snapshot_reports_replica_and_validation_warning() {
    if !isolated_run_observation(
        "run_observation::readiness_snapshot_reports_replica_and_validation_warning",
    ) {
        return;
    }
    let fixture = Fixture::terminal();
    let idle = fixture.json(&["run", "readiness", "--json"]);
    assert_eq!(idle["replica"], false);
    assert!(
        idle.get("validation_env_warning")
            .is_some_and(Value::is_null)
    );
    for (key, value) in [
        ("workflow.validation_env.shell_probe", "off"),
        (
            "workflow.required_validation_commands",
            r#"["make ci-fast"]"#,
        ),
    ] {
        fixture.config_set(key, value);
    }
    let warned = fixture.json(&["run", "readiness", "--json"]);
    let warning = warned["validation_env_warning"]
        .as_str()
        .expect("validation warning");
    assert!(fixture.readiness_text().contains(warning));
}
#[test]
#[cfg(unix)]
fn readiness_reuses_validation_warning_across_processes() {
    if !isolated_run_observation(
        "run_observation::readiness_reuses_validation_warning_across_processes",
    ) {
        return;
    }
    let fixture = Fixture::terminal();
    let marker = fixture.home.join("login-probes");
    let probes = || fs::read_to_string(&marker).unwrap().lines().count();
    let profile = "printf 'probe\n' >> \"$HOME/login-probes\"\nexport PATH=\"$HOME/login-tools:/usr/bin:/bin\"\n";
    for name in [".bash_profile", ".profile", ".zprofile"] {
        fs::write(fixture.home.join(name), profile).unwrap();
    }
    fs::create_dir_all(fixture.home.join(".config/fish")).unwrap();
    fs::write(fixture.home.join(".config/fish/config.fish"), "printf 'probe\n' >> \"$HOME/login-probes\"\nset -gx PATH \"$HOME/login-tools\" /usr/bin /bin\n").unwrap();
    let config = fixture.work.join(".orbit/config.toml");
    let configure = |mode: &str| {
        fs::write(&config, format!("[workflow]\nrequired_validation_commands = [\"make check\"]\n[workflow.validation_env]\npath = [\"/usr/bin\", \"/bin\"]\npath_mode = \"{mode}\"\n")).unwrap()
    };
    configure("replace");
    let first = fixture.json(&["run", "readiness", "--json"]);
    let first_warning = first["validation_env_warning"].clone();
    let warning = first_warning.as_str().expect("missing PATH warning");
    contains(warning, fixture.home.join("login-tools").to_str().unwrap());
    assert_eq!(probes(), 1);
    assert_eq!(
        fixture.json(&["run", "readiness", "--json"])["validation_env_warning"],
        first_warning
    );
    assert_eq!(
        probes(),
        1,
        "a fresh readiness process must not rerun the login shell within the diagnostic TTL"
    );
    configure("prepend");
    let kept = fixture.json(&["run", "readiness", "--json"]);
    assert!(kept["validation_env_warning"].is_null());
    assert_eq!(probes(), 2);
    let cache = fixture
        .work
        .join(".orbit/state/validation-env-warning.json");
    let expired = std::time::SystemTime::now()
        - orbit_exec::LOGIN_SHELL_CACHE_TTL
        - std::time::Duration::from_secs(1);
    fs::File::options()
        .write(true)
        .open(&cache)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(expired))
        .unwrap();
    assert_eq!(
        fixture.json(&["run", "readiness", "--json"])["validation_env_warning"],
        kept["validation_env_warning"]
    );
    assert_eq!(probes(), 3);
    configure("replace");
    fs::write(&cache, "incomplete JSON").unwrap();
    assert_eq!(
        fixture.json(&["run", "readiness", "--json"])["validation_env_warning"],
        first_warning
    );
    assert_eq!(probes(), 4);
    fs::rename(&cache, fixture.home.join("saved-warning.json")).unwrap();
    fs::create_dir(&cache).unwrap();
    for expected in [5, 6] {
        assert_eq!(
            fixture.json(&["run", "readiness", "--json"])["validation_env_warning"],
            first_warning
        );
        assert_eq!(probes(), expected);
    }
}
fn readiness_reports_total_and_notifies_only_when_backlog_is_truncated() {
    use orbit_core::TaskStatus;
    let fixture = Fixture::init();
    let runtime = fixture.runtime();
    for index in 0..3 {
        runtime
            .add_task(task_params(
                format!("readiness candidate {index}"),
                "Readiness total fixture".into(),
                TaskStatus::Backlog,
            ))
            .unwrap();
    }
    let limited_text = fixture.text(&["run", "readiness", "--limit", "2"]);
    assert_eq!(
        limited_text.lines().last(),
        Some("showing 2 of 3 backlog tasks; use --limit N or name task ids"),
        "the truncation notice must be the final text line: {limited_text}"
    );
    let limited_json = fixture.json(&["run", "readiness", "--limit", "2", "--json"]);
    assert_eq!(limited_json["total"], 3);
    assert_eq!(limited_json["tasks"].as_array().unwrap().len(), 2);
    assert!(
        !fixture
            .text(&["run", "readiness", "--limit", "3"])
            .contains("showing "),
        "a complete view must not print a truncation notice"
    );
    let complete_json = fixture.json(&["run", "readiness", "--limit", "3", "--json"]);
    assert_eq!(complete_json["total"], 3);
    assert_eq!(
        complete_json["total"].as_u64().unwrap() as usize,
        complete_json["tasks"].as_array().unwrap().len()
    );
}
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
    let source = fixture.show_json(FAILED);
    let replay = fixture.json(&["job", "replay", FAILED, "--json"]);
    assert_eq!(replay["success"], true);
    assert_eq!(replay["source_run_id"], FAILED);
    let id = replay["run_id"].as_str().unwrap();
    assert_ne!(id, FAILED);
    let stored = fixture.show_json(id);
    assert_eq!(stored["run"]["state"], "success");
    assert_eq!(stored["run"]["retry_source_run_id"], FAILED);
    let persisted: String = fixture
        .db()
        .query_row(
            "SELECT input_json FROM job_runs WHERE run_id=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&persisted).unwrap(), input);
    assert_eq!(fixture.show_json(FAILED), source);
    fixture
        .orbit()
        .args(["job", "replay", "jrun-missing-fixture", "--json"])
        .assert()
        .failure();
    assert_eq!(fixture.show_json(FAILED), source);
}
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
    let states: Vec<_> = history["runs"]
        .as_array()
        .expect("runs")
        .iter()
        .map(|run| {
            (
                run["run_id"].as_str().unwrap().to_string(),
                run["state"].as_str().unwrap().to_string(),
            )
        })
        .collect();
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
    let shown = fixture.show_json(FAILED);
    assert_eq!(shown["run"]["state"], "failed");
    assert!(
        shown["steps"]
            .as_array()
            .expect("steps")
            .iter()
            .any(|step| step["state"] == "failed" && step["error_code"] == "fixture_error"),
        "show must return the step failure: {shown}"
    );
    for run_id in [STALE_RUNNING, STALE_PENDING] {
        assert!(
            matches!(
                fixture.show_json(run_id)["run"]["state"].as_str(),
                Some("running" | "pending")
            ),
            "show must report {run_id} as stored"
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
        ]
    };
    for args in lookups(FAILED) {
        let error = fixture.failure(&args);
        assert_eq!(error["code"], "store_error", "{args:?} -> {error}");
        assert!(
            error["error"]
                .as_str()
                .is_some_and(|m| m.contains("corrupt_state")),
            "{args:?} must keep the store diagnostic: {error}"
        );
    }
    for args in lookups(MISSING) {
        let error = fixture.failure(&args);
        assert_eq!(error["code"], "job_run_not_found", "{args:?} -> {error}");
        assert!(
            error["error"].as_str().is_some_and(|m| m.contains(MISSING)),
            "{args:?} must name the missing run: {error}"
        );
    }
}
/// `orbit run events` reads the complete trail in its enriched projection:
/// a step selector resolves by audit id or index, `--type` is exact, a child
/// event carries the `step_id` it inherits, and payloads that are not JSON
/// or carry no `event_id` are dropped.
fn run_events_select_resolved_steps_and_add_inherited_step_ids() {
    let fixture = Fixture::init();
    let workspace_id = fixture.workspace_id();
    let db = fixture.db();
    let rows = [
        (
            "e0",
            r#"{"event_id":"e0","body_kind":"step_started","event_type":"step.started","step_id":"implement"}"#,
        ),
        (
            "e1",
            r#"{"event_id":"e1","parent_event_id":"e0","body_kind":"cli_invocation_finished","event_type":"cli.invocation.finished"}"#,
        ),
        ("bad", "not json"),
        (
            "anon",
            r#"{"body_kind":"activity_progress","event_type":"activity.progress"}"#,
        ),
        (
            "e2",
            r#"{"event_id":"e2","body_kind":"step_started","event_type":"step.started","step_id":"review"}"#,
        ),
        (
            "e3",
            r#"{"event_id":"e3","parent_event_id":"e2","body_kind":"cli_invocation_finished","event_type":"cli.invocation.finished"}"#,
        ),
    ];
    for (second, (event_id, payload)) in rows.into_iter().enumerate() {
        let ts = format!("2026-09-20T03:01:{second:02}+00:00");
        db.execute("INSERT INTO v2_audit_events (workspace_id, event_id, source, schema_version, event_type, ts, run_id, agent_identity, payload_json) VALUES (?1, ?2, 'v2_envelope', 1, 'fixture', ?3, ?4, 'test', ?5)", params![workspace_id, event_id, ts, FAILED, payload]).expect("seed audit event");
    }
    let ids = |doc: &Value| -> Vec<String> {
        doc["events"]
            .as_array()
            .expect("events")
            .iter()
            .map(|event| event["event_id"].as_str().unwrap_or("-").to_string())
            .collect()
    };

    let all = fixture.json(&["run", "events", FAILED, "--no-reconcile", "--json"]);
    assert_eq!(all["run_id"], FAILED);
    assert_eq!(all["job_id"], "fixture_job");
    assert_eq!(
        ids(&all),
        ["e0", "e1", "e2", "e3"],
        "chronological, malformed and id-less rows dropped: {all}"
    );
    assert_eq!(
        all["events"][1]["step_id"], "implement",
        "child event inherits its step: {all}"
    );

    let by_index = fixture.json(&[
        "run",
        "events",
        FAILED,
        "-s",
        "1",
        "--type",
        "cli.invocation.finished",
        "--no-reconcile",
        "--json",
    ]);
    assert_eq!(ids(&by_index), ["e3"], "{by_index}");
    assert_eq!(by_index["events"][0]["step_id"], "review");

    let by_id = fixture.json(&[
        "run",
        "events",
        FAILED,
        "--step",
        "implement",
        "--type",
        "step.started",
        "--no-reconcile",
        "--json",
    ]);
    assert_eq!(ids(&by_id), ["e0"], "{by_id}");

    let error = fixture.failure(&[
        "run",
        "events",
        FAILED,
        "-s",
        "missing",
        "--no-reconcile",
        "--json",
    ]);
    assert!(
        error["error"]
            .as_str()
            .is_some_and(|m| m.contains("does not match any step")),
        "an unknown step selector is refused: {error}"
    );
}
fn job_run_alias_produces_a_completed_trace_and_terminal_cancel_is_stable() {
    let fixture = Fixture::init();
    let jobs = fixture.home.join(".orbit/resources/jobs");
    fs::create_dir_all(&jobs).unwrap();
    fs::write(jobs.join("alias_fixture.yaml"), "schemaVersion: 2\nkind: Job\nmetadata:\n  name: alias_fixture\nspec:\n  state: enabled\n  kind: workflow\n  steps:\n    - id: nap\n      default_input:\n        seconds: 0\n      spec:\n        type: deterministic\n        action: sleep\n        config: {}\n").unwrap();
    let completed = fixture.json(&["job", "run", "alias_fixture", "--wait", "--json"]);
    assert_eq!(completed["state"], "success");
    assert_eq!(completed["wait_timeout"], false);
    let run_id = completed["run_id"].as_str().unwrap();
    assert_eq!(fixture.show_json(run_id)["run"]["state"], "success");
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
        assert_eq!(fixture.json(&args)["outcome"], "cancelled");
        let task = fixture.json(&["task", "show", &task_id, "--json"]);
        assert_eq!(task["status"], expected_status);
        assert_eq!(task["plan"], "Resume the candidate after cancellation");
        assert_eq!(task["context_files"][0], "file:src/candidate.rs");
        assert_eq!(
            fs::read_to_string(&candidate).unwrap(),
            "candidate content\n"
        );
        let runtime = fixture.runtime();
        let policy = runtime
            .read_run_state(run_id)
            .expect("run state")
            .and_then(|s| s.task_cancellation_policy)
            .expect("durable cancellation policy");
        assert_eq!(policy.block, block);
        let history = runtime.get_task_history(&task_id).expect("task history");
        let entry = history.last().expect("cancellation status event");
        assert_eq!(
            entry.from_status,
            Some(orbit_types::task::TaskStatus::InProgress)
        );
        assert_eq!(
            entry.to_status.map(|s| s.to_string()).as_deref(),
            Some(expected_status)
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
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn ps_lstart_utc(test: &str, pid: u32) -> Option<String> {
    let output = match test_env::ps_lstart_utc(pid) {
        test_env::PsRun::Ran(output) => output,
        test_env::PsRun::Denied(reason) => {
            test_env::report_ps_denied(test, &reason);
            return None;
        }
    };
    assert!(output.status.success(), "ps fixture: {output:?}");
    let raw = String::from_utf8(output.stdout).unwrap().trim().to_string();
    assert!(!raw.is_empty(), "ps must describe the live fixture process");
    Some(raw)
}
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
    let Some(raw) = ps_lstart_utc(
        "run_observation::linux_start_identity_without_ps_matches_pre_change_owner_tokens",
        pid,
    ) else {
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
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn cancelling_owners_with_pre_change_ps_tokens_still_signals_them() {
    use orbit_common::process::identity::{
        STABLE_TOKEN_PREFIX, STABLE_TOKEN_PREFIX_V1, current_pid_namespace, process_is_alive,
    };
    let fixture = Fixture::init();
    for format in ["v2", "v1", "legacy"] {
        let owner = test_env::spawn_unrelated_process();
        let Some(raw) = ps_lstart_utc(
            "run_observation::cancelling_owners_with_pre_change_ps_tokens_still_signals_them",
            owner.pid(),
        ) else {
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
        fixture.db().execute("INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state, scheduled_at, started_at, created_at, pid, pid_start_time) VALUES (?1, ?2, 'fixture_job', 1, 'running', ?3, ?3, ?3, ?4, ?5)", params![run_id, fixture.workspace_id(), now, owner.pid(), token]).unwrap();
        let cancelled = fixture.json(&["run", "cancel", &run_id, "--confirm", "--json"]);
        assert_eq!(cancelled["outcome"], "cancelled", "{format}: {cancelled}");
        assert!(
            cancelled["signal_outcome"]
                .as_str()
                .is_some_and(|o| o.starts_with("terminated_")),
            "pre-change {format} owner token must still verify: {cancelled}"
        );
        assert!(
            !process_is_alive(owner.pid()),
            "{format} owner must be stopped"
        );
    }
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn cancelling_a_live_run_keeps_the_signalled_worker_exit_as_its_outcome() {
    use std::process::{Child, Stdio};
    use std::time::{Duration, Instant};
    struct ReapOnDrop(Child);
    impl Drop for ReapOnDrop {
        fn drop(&mut self) {
            if matches!(self.0.try_wait(), Ok(None)) {
                let _ = self.0.kill();
            }
            let _ = self.0.wait();
        }
    }
    struct KillWorkerGroup(Option<libc::pid_t>);
    impl Drop for KillWorkerGroup {
        fn drop(&mut self) {
            if let Some(pid) = self.0 {
                unsafe { libc::kill(-pid, libc::SIGKILL) };
            }
        }
    }
    let fixture = Fixture::init();
    let jobs = fixture.home.join(".orbit/resources/jobs");
    fs::create_dir_all(&jobs).unwrap();
    fs::write(jobs.join("cancel_fixture.yaml"), "schemaVersion: 2\nkind: Job\nmetadata:\n  name: cancel_fixture\nspec:\n  state: enabled\n  kind: workflow\n  steps:\n    - id: nap\n      default_input:\n        seconds: 60\n      spec:\n        type: deterministic\n        action: sleep\n        config: {}\n").unwrap();
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
    assert_eq!(cancelled["outcome"], "cancelled");
    assert_eq!(cancelled["previous_state"], "running");
    assert_eq!(cancelled["final_state"], "cancelled");
    assert_eq!(cancelled["signal_attempted"], true);
    assert!(
        cancelled["signal_outcome"]
            .as_str()
            .is_some_and(|o| o.starts_with("terminated_")),
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
    assert_eq!(fixture.show_json(&run_id)["run"]["state"], "cancelled");
    let step_errors: Vec<String> = {
        let db = fixture.db();
        let mut st = db
            .prepare("SELECT IFNULL(error_code, '-') FROM job_run_steps WHERE run_id = ?1")
            .unwrap();
        st.query_map([&run_id], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    for code in ["worker_terminated", "terminal_outcome_conflict"] {
        assert!(
            !step_errors.iter().any(|e| e == code),
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
    assert!(exit["exit_status"].as_str().is_some_and(|s| !s.is_empty()));
    let repeated = fixture.json(&["run", "cancel", &run_id, "--confirm", "--json"]);
    assert_eq!(repeated["outcome"], "already_terminal");
    assert_eq!(repeated["final_state"], "cancelled");
    assert_eq!(repeated["signal_attempted"], false);
    assert_eq!(fixture.run_state(&run_id), "cancelled");
}
fn replacement_drain_counts_and_reports_inherited_workers_until_they_finish() {
    use orbit_core::TaskStatus;
    use orbit_types::workflow::PipelineState;
    use serde_json::json;
    let fixture = Fixture::init();
    let runtime = fixture
        .runtime()
        .with_host_resource_probe(std::sync::Arc::new(CalmHost));
    let tasks: Vec<_> = (0..7)
        .map(|index| {
            let file = format!("task-{index}.rs");
            fs::write(fixture.work.join(&file), "fixture\n").unwrap();
            {
                let mut params = task_params(
                    format!("independent task {index}"),
                    "Fixture delivery".into(),
                    if index < 2 {
                        TaskStatus::InProgress
                    } else {
                        TaskStatus::Backlog
                    },
                );
                params.acceptance_criteria = vec!["Delivery completes".into()];
                params.plan = "Fixture plan".into();
                params.context_files = vec![format!("file:{file}")];
                runtime.add_task(params)
            }
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
        fixture.db().execute("INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,input_json,scheduled_at,started_at,created_at,pid) VALUES (?1,?2,?3,1,?4,?5,?6,?6,?6,?7)", params![id, fixture.workspace_id(), job, status, input.to_string(), now.to_rfc3339(), std::process::id()]).unwrap();
        let mut state = PipelineState::new(id.clone(), job.into(), input);
        for (child, child_job) in children {
            dispatch(&mut state, child, child_job, "dispatch", now);
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
    let stopped = fixture.json(&["run", "auto", "--stop", "--json"]);
    let coordinator = &stopped["coordinators"][0];
    assert_eq!(coordinator["run_id"], old);
    assert_eq!(coordinator["job_id"], "workspace_auto_pipeline");
    assert_eq!(
        coordinator["forced_runs"],
        json!([]),
        "the CLI returns the stop projection MCP and the dashboard return: {stopped}"
    );
    assert!(coordinator["remaining_children"].as_array().is_some_and(
        |children| !children.is_empty()
            && children.iter().all(|child| {
                ["run_id", "job_name", "phase", "child_status"]
                    .iter()
                    .all(|key| child.get(key).is_some())
            })
    ));
    assert!(
        runtime
            .read_run_state(&old)
            .unwrap()
            .unwrap()
            .pull_drain
            .drain_admissions_stop
            .is_some()
    );
    fixture.set_run_state(&old, "success");
    let replacement = seed(
        "workspace_auto_pipeline",
        "running",
        json!({"max_active_leaf_runs": 3}),
        &[],
    );
    let classify = || {
        run_det(
            &runtime,
            "classify_workspace_auto_tasks",
            json!({"run_id": replacement, "max_active_leaf_runs": 3, "mode": "pr"}),
        )
    };
    let verify = |occupied: u64, inherited: u64, offered: usize, limit: u64| {
        let wave = classify();
        assert_eq!(wave["active_leaf_runs"], occupied);
        assert_eq!(wave["inherited_leaf_runs"], inherited);
        assert_eq!(wave["free_slots"], limit.saturating_sub(occupied));
        assert_eq!(wave["loose_task_ids"].as_array().unwrap().len(), offered);
        let shown = fixture.show_json(&replacement);
        let capacity = json!({"active_leaf_runs": occupied, "inherited_leaf_runs": inherited, "max_active_leaf_runs": limit});
        assert_eq!(
            shown["pipeline_state"]["drain_last_pass"]["capacity"],
            capacity
        );
        assert_eq!(shown["drain_summary"]["capacity"], capacity);
        contains(
            &fixture.show_text(&replacement),
            &format!("occupied={occupied} inherited={inherited} limit={limit}"),
        );
        wave
    };
    fixture.conc(&replacement, "1", None, None);
    verify(2, 2, 0, 1);
    fixture.conc(&replacement, "3", None, None);
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
    dispatch(
        &mut state,
        &own_wrapper,
        "task_auto_pipeline",
        "dispatch",
        chrono::Utc::now(),
    );
    runtime.write_run_state(&replacement, &state).unwrap();
    verify(3, 2, 0, 3);
    fixture.set_run_state(&own_wrapper, "success");
    verify(3, 2, 0, 3);
    for id in [&old_wrapper, &old_leaf] {
        fixture.set_run_state(id, "success");
    }
    verify(2, 1, 1, 3);
    fixture.set_run_state(&detached_leaf, "success");
    verify(1, 0, 2, 3);
    let shown = fixture.show_json(&replacement);
    assert_eq!(shown["drain_summary"]["admitted"], 1);
    assert_eq!(shown["drain_summary"]["succeeded"], 1);
    assert_eq!(shown["drain_summary"]["failed"], 0);
}
fn run_concurrency_updates_persisted_revision_and_refuses_stale_writes() {
    let fixture = Fixture::init();
    let id = "jrun-cli-workers";
    let input = serde_json::json!({"max_active_leaf_runs": 2});
    let (runtime, _) = fixture.seed_running(id, "workspace_auto_pipeline", input.clone());
    let changed = fixture.conc(id, "3", Some("0"), Some("CLI fixture"));
    assert_eq!(changed["outcome"], "updated");
    assert_eq!(changed["previous_concurrency"], 2);
    assert_eq!(changed["concurrency"], 3);
    assert_eq!(changed["revision"], 1);
    assert_eq!(fixture.show_json(id)["run"]["state"], "running");
    let before = runtime.read_run_state(id).unwrap().unwrap();
    let limit = before
        .pull_drain
        .drain_worker_limit
        .as_ref()
        .expect("persisted CLI worker limit");
    assert_eq!(limit.max_active_leaf_runs, 3);
    assert_eq!(limit.previous_max_active_leaf_runs, 2);
    assert_eq!(limit.revision, 1);
    assert_eq!(limit.actor, "cli");
    assert_eq!(limit.reason.as_deref(), Some("CLI fixture"));
    let unchanged = fixture.conc(id, "3", Some("1"), None);
    assert_eq!(unchanged["outcome"], "unchanged");
    assert_eq!(unchanged["revision"], 1);
    let error = fixture.conc_err(id, "4", Some("0"));
    assert!(error["error"].as_str().unwrap().contains("revision"));
    assert_eq!(runtime.read_run_state(id).unwrap().unwrap(), before);
    assert_eq!(runtime.show_job_run(id).unwrap().input, Some(input));
    assert_eq!(fixture.run_state(id), "running");
}
fn run_extend_lengthens_a_live_drain_and_a_repeat_start_still_queues() {
    let fixture = Fixture::init();
    orbit_core::test_support::install_substitute_pipeline_worker(["sh", "-c", "exit 0"]);
    let id = "jrun-cli-extend";
    let input = serde_json::json!({"for_seconds": 3600, "max_active_leaf_runs": 2});
    let now = chrono::Utc::now();
    let (runtime, mut state) = fixture.seed_running(id, "workspace_auto_pipeline", input);
    let stamped =
        (now + chrono::Duration::minutes(10)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    state.record_pipeline_output("open_window", serde_json::json!({"deadline": stamped}));
    runtime.write_run_state(id, &state).unwrap();
    let changed = fixture.extend(id, "3h", Some("4"), Some("CLI fixture"));
    assert_eq!(changed["outcome"], "extended");
    assert_eq!(changed["previous_deadline"], stamped.as_str());
    assert_eq!(changed["revision"], 1);
    assert_eq!(changed["concurrency"]["previous_concurrency"], 2);
    assert_eq!(changed["concurrency"]["concurrency"], 4);
    let extended = runtime.read_run_state(id).unwrap().unwrap();
    let extension = extended.pull_drain.drain_window_extension.as_ref().unwrap();
    assert_eq!(
        changed["deadline"],
        extension
            .deadline
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    );
    assert!(extension.deadline >= now + chrono::Duration::hours(3));
    assert_eq!(extension.actor, "cli");
    assert_eq!(extension.reason.as_deref(), Some("CLI fixture"));
    assert_eq!(
        extended
            .pull_drain
            .drain_worker_limit
            .as_ref()
            .unwrap()
            .max_active_leaf_runs,
        4
    );
    assert_eq!(
        fixture.show_json(id)["run"]["drain_window_extension"]["revision"],
        1
    );
    let unchanged = fixture.extend(id, "5m", None, None);
    assert_eq!(unchanged["outcome"], "unchanged");
    assert_eq!(unchanged["deadline"], changed["deadline"]);
    assert!(unchanged["concurrency"].is_null());
    let error = fixture.extend_err(id, "soon", None);
    assert!(error["error"].as_str().unwrap().contains("duration"));
    let invalid = fixture.extend_err(id, "4h", Some("0"));
    assert!(
        invalid["error"]
            .as_str()
            .unwrap()
            .contains("worker ceiling must be at least 1")
    );
    assert_eq!(runtime.read_run_state(id).unwrap().unwrap(), extended);
    let successor = runtime
        .submit_pipeline_run(
            "workspace_auto_pipeline",
            serde_json::json!({"for_seconds": 7200}),
            None,
            Some("fixture"),
        )
        .unwrap();
    assert!(successor.queued);
    assert_eq!(runtime.read_run_state(id).unwrap().unwrap(), extended);
    assert_eq!(fixture.run_state(id), "running");
}
fn a_drain_at_concurrency_twenty_runs_twenty_delivery_wrappers_at_once() {
    let fixture = Fixture::init();
    orbit_core::test_support::install_substitute_pipeline_worker(["sh", "-c", "exit 0"]);
    let runtime = fixture.runtime();
    let now = chrono::Utc::now().to_rfc3339();
    for index in 0..19 {
        fixture.db().execute("INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,scheduled_at,started_at,created_at,pid) VALUES (?1,?2,'task_auto_pipeline',1,'running',?3,?3,?3,?4)", params![format!("jrun-cli-wrapper-{index}"), fixture.workspace_id(), now, std::process::id()]).unwrap();
    }
    let twentieth = runtime
        .submit_pipeline_run(
            "task_auto_pipeline",
            serde_json::json!({"task_ids": []}),
            None,
            Some("fixture"),
        )
        .unwrap();
    assert!(!twentieth.queued);
    assert_eq!(twentieth.queue_position, None);
}
fn run_concurrency_retunes_a_pull_drain_past_the_former_leaf_ceiling() {
    let fixture = Fixture::init();
    let id = "jrun-cli-pull-workers";
    let input = serde_json::json!({"max_active_leaf_runs": 5});
    let (runtime, _) = fixture.seed_running(id, "workspace_pull_pipeline", input.clone());
    let changed = fixture.conc(id, "24", None, None);
    assert_eq!(changed["outcome"], "updated");
    assert_eq!(changed["job_id"], "workspace_pull_pipeline");
    assert_eq!(changed["previous_concurrency"], 5);
    assert_eq!(changed["concurrency"], 24);
    let state = runtime.read_run_state(id).unwrap().unwrap();
    assert_eq!(state.effective_max_active_leaf_runs(5), 24);
    assert_eq!(runtime.show_job_run(id).unwrap().input, Some(input));
    let refused = fixture.conc_err(FAILED, "24", None);
    assert!(refused["error"].as_str().unwrap().contains("fixture_job"));
}
fn readiness_uses_a_live_pull_drains_ceiling() {
    let fixture = Fixture::init();
    let idle = fixture.json(&["run", "readiness", "--json"]);
    for (key, expected) in [
        ("max_active_leaf_runs", serde_json::json!(5)),
        ("active_leaf_runs", serde_json::json!(0)),
        ("free_slots", serde_json::json!(5)),
        ("limit_source", serde_json::json!("default")),
        ("drain_run_id", serde_json::Value::Null),
        ("pull_drain_run_id", serde_json::Value::Null),
    ] {
        assert_eq!(cap(&idle)[&key], expected);
    }
    let idle_text = fixture.readiness_text();
    contains(&idle_text, "Active leaf runs: 0/5; free slots: 5.");
    assert!(
        !idle_text.contains("Leaf ceiling:"),
        "idle readiness must not name a pull ceiling: {idle_text}"
    );
    let pull_id = "jrun-cli-pull-ceiling";
    fixture.seed_running(
        pull_id,
        "workspace_pull_pipeline",
        serde_json::json!({"max_active_leaf_runs": 3}),
    );
    let now = chrono::Utc::now().to_rfc3339();
    for index in 0..2 {
        fixture.db().execute("INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,scheduled_at,started_at,created_at,pid) VALUES (?1,?2,'task_pr_pipeline',1,'running',?3,?3,?3,?4)", params![format!("jrun-cli-leaf-ceiling-{index}"), fixture.workspace_id(), now, std::process::id()]).unwrap();
    }
    let submitted = fixture.json(&["run", "readiness", "--json"]);
    for (key, expected) in [
        ("max_active_leaf_runs", serde_json::json!(3)),
        ("active_leaf_runs", serde_json::json!(2)),
        ("free_slots", serde_json::json!(1)),
        ("limit_source", serde_json::json!("pull_run_input")),
        ("pull_drain_run_id", serde_json::json!(pull_id)),
        ("drain_run_id", serde_json::Value::Null),
    ] {
        assert_eq!(cap(&submitted)[&key], expected);
    }
    let submitted_text = fixture.readiness_text();
    contains(&submitted_text, "Active leaf runs: 2/3; free slots: 1.");
    contains(
        &submitted_text,
        &format!("Leaf ceiling: {pull_id} (pull_run_input)."),
    );
    let changed = fixture.conc(pull_id, "4", None, None);
    assert_eq!(changed["outcome"], "updated");
    assert_eq!(changed["concurrency"], 4);
    let resized = fixture.json(&["run", "readiness", "--json"]);
    for (key, expected) in [
        ("max_active_leaf_runs", serde_json::json!(4)),
        ("active_leaf_runs", serde_json::json!(2)),
        ("free_slots", serde_json::json!(2)),
        ("limit_source", serde_json::json!("pull_run_control")),
    ] {
        assert_eq!(cap(&resized)[&key], expected);
    }
    let resized_text = fixture.readiness_text();
    contains(&resized_text, "Active leaf runs: 2/4; free slots: 2.");
    contains(
        &resized_text,
        &format!("Leaf ceiling: {pull_id} (pull_run_control)."),
    );
    let requested = fixture.json(&["run", "readiness", "--concurrency", "8", "--json"]);
    for (key, expected) in [
        ("max_active_leaf_runs", serde_json::json!(8)),
        ("free_slots", serde_json::json!(6)),
        ("limit_source", serde_json::json!("requested")),
    ] {
        assert_eq!(cap(&requested)[&key], expected);
    }
    let local_id = "jrun-cli-local-ceiling";
    fixture.seed_running(
        local_id,
        "workspace_auto_pipeline",
        serde_json::json!({"max_active_leaf_runs": 7}),
    );
    let local = fixture.json(&["run", "readiness", "--json"]);
    assert_eq!(
        cap(&local)["max_active_leaf_runs"],
        7,
        "a live local drain keeps precedence over the pull drain: {local}"
    );
    for (key, expected) in [
        ("active_leaf_runs", serde_json::json!(2)),
        ("free_slots", serde_json::json!(5)),
        ("limit_source", serde_json::json!("run_input")),
        ("drain_run_id", serde_json::json!(local_id)),
        ("pull_drain_run_id", serde_json::json!(pull_id)),
    ] {
        assert_eq!(cap(&local)[&key], expected);
    }
    let local_text = fixture.readiness_text();
    contains(&local_text, "Active leaf runs: 2/7; free slots: 5.");
    assert!(
        !local_text.contains("Leaf ceiling:"),
        "local-drain text must not name the pull ceiling: {local_text}"
    );
    contains(&local_text, &format!("Running drain: {local_id}."));
    contains(&local_text, &format!("Running pull drain: {pull_id}."));
}
fn a_live_drains_recorded_throttle_reaches_readiness_run_show_and_ship() {
    let fixture = Fixture::init();
    let id = "jrun-cli-throttled-pull";
    let now = chrono::Utc::now();
    let (runtime, mut state) =
        fixture.seed_running(id, "workspace_pull_pipeline", serde_json::json!({}));
    let since = chrono::DateTime::parse_from_rfc3339("2026-10-04T08:41:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let mut pass = pass_at(now);
    pass.resource_throttle = Some(orbit_types::workflow::ResourceThrottle {
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
    });
    state.pull_drain.drain_last_pass = Some(pass);
    runtime.write_run_state(id, &state).unwrap();
    let held = "Admissions throttled: memory 93% (throttled at \u{2265} 90% since 2026-10-04 08:41Z; resumes below 80%); cpu 89% (throttled at \u{2265} 90% since 2026-10-04 08:41Z; resumes below 75%)";
    let readiness = fixture.json(&["run", "readiness", "--json"]);
    assert_eq!(cap(&readiness)["resource_cpu_source"], "load");
    assert_eq!(cap(&readiness)["free_slots"], 0);
    assert_eq!(
        cap(&readiness)["resource_throttle"]["resources"][0]["percent"],
        93.2
    );
    assert_eq!(
        cap(&readiness)["resource_throttle"]["resources"][1]["percent"],
        89.0
    );
    let text = fixture.readiness_text();
    contains(&text, held);
    assert!(
        !text.contains("89% \u{2265}"),
        "reading inside hysteresis band must not print a false comparison: {text}"
    );
    contains(&fixture.show_text(id), &format!("Throttled: {held}"));
    let refused = fixture.failure(&["run", "ship", "--json"]);
    assert!(
        refused["error"]
            .as_str()
            .is_some_and(|e| e.contains("resource_throttled") && e.contains(held))
    );
    for (key, value) in [
        ("workflow.resource_throttle.cpu_source", "utilization"),
        ("workflow.resource_throttle.enabled", "false"),
    ] {
        fixture.config_set(key, value);
    }
    let configured = fixture.json(&["run", "readiness", "--json"]);
    assert_eq!(cap(&configured)["resource_cpu_source"], "utilization");
    contains(
        &fixture.readiness_text(),
        "Resource throttle CPU source: utilization.",
    );
    assert_eq!(
        fixture.doctor_row("resource-throttle")["message"],
        "CPU source: utilization"
    );
}
fn run_show_counts_local_deferred_tasks_once() {
    let fixture = Fixture::init();
    let id = "jrun-cli-local-deferred";
    let now = chrono::Utc::now();
    let completed = now - chrono::Duration::minutes(1);
    let lock_error = "record pilot operator handoffs: timed out after 30000ms acquiring task commit boundary (held by pid 4242)";
    let mut pass = pass_at(now);
    pass.queued = 3;
    pass.deferred = (1..=3)
        .map(|n| {
            waiting(
                &format!("ORB-1520{n}"),
                "context_lock_conflict",
                &["ORB-15183"],
                None,
            )
        })
        .collect();
    pass.deferred_total = 3;
    pass.waiting_recorded_at = Some(completed);
    pass.last_pass_error_code = Some("lock_busy".into());
    pass.last_pass_error = Some(lock_error.into());
    pass.consecutive_pass_failures = 1;
    fixture.seed_pass(id, "workspace_auto_pipeline", pass);
    let shown = fixture.show_json(id);
    let pass = &shown["pipeline_state"]["drain_last_pass"];
    assert_eq!(pass["queued"], 3);
    assert_eq!(pass["deferred_total"], 3);
    let text = fixture.show_text(id);
    contains(
        &text,
        "Still waiting: 0 admissible, 3 deferred and 0 excluded backlog task(s) were never started at the last pass",
    );
    assert!(
        !text.contains("Still waiting: 3 admissible, 3 deferred"),
        "local deferred tasks must not be counted twice: {text}"
    );
    let dated = format!(
        "(as of the last completed pass at {})",
        completed.format("%Y-%m-%d %H:%M:%SZ")
    );
    assert!(
        text.contains(&dated) && !text.contains("the owner answered"),
        "a local drain's kept lists are dated from its last completed pass: {text}"
    );
    assert!(text.contains(&format!(
        "Admission pass deferred: consecutive_failures=1 last_pass_error={lock_error}"
    )));
}
fn run_show_names_the_tasks_a_pull_drains_owner_kept_off_this_host() {
    let fixture = Fixture::init();
    let id = "jrun-cli-pull-waiting";
    let now = chrono::Utc::now();
    let answered = chrono::DateTime::parse_from_rfc3339("2026-10-07T06:44:53Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let mut pass = pass_at(now);
    pass.queued = 9;
    pass.deferred = vec![
        waiting("ORB-101", "context_lock_conflict", &["ORB-900"], None),
        waiting(
            "ORB-102",
            "owner_hold",
            &[],
            Some("held for a red base: make ci-lint"),
        ),
    ];
    pass.deferred_total = 2;
    pass.excluded = [
        ("ORB-103", "dependency_not_done", &["ORB-901"][..], None),
        ("ORB-104", "host_os_mismatch", &[][..], Some("waits for a linux host (os:linux); the executor runs macos")),
        ("ORB-105", "crew_unavailable", &[][..], Some("crew antigravity cannot run on this host")),
        ("ORB-106", "native_os_required", &[][..], Some("Native OS requirement: criterion 2 needs native macos evidence, and the task's tags lack `os:macos`")),
        ("ORB-107", "browser_kit_missing", &[][..], Some("waits for a host with the browser kit (ui); run `orbit browser-kit install` on a host that runs workers to give it one; the executor reports no verified kit")),
    ].into_iter().map(|(task, reason, blocked, detail)| waiting(task, reason, blocked, detail)).collect();
    pass.excluded_total = 5;
    pass.waiting_recorded_at = Some(answered);
    pass.waiting_by_reason = [
        ("context_lock_conflict", 4),
        ("owner_hold", 11),
        ("dependency_not_done", 1),
        ("host_os_mismatch", 1),
        ("crew_unavailable", 1),
        ("native_os_required", 1),
        ("browser_kit_missing", 1),
    ]
    .into_iter()
    .map(|(r, c)| (r.to_string(), c))
    .collect();
    pass.consecutive_idle_passes = 4;
    let (runtime, mut state) = fixture.seed_pass(id, "workspace_pull_pipeline", pass);
    let shown = fixture.show_json(id);
    let pass = &shown["pipeline_state"]["drain_last_pass"];
    assert_eq!(pass["queued"], 9);
    assert_eq!(pass["excluded_total"], 5);
    assert_eq!(pass["deferred"][0]["blocked_by"][0], "ORB-900");
    assert_eq!(pass["excluded"][0]["reason"], "dependency_not_done");
    assert_eq!(pass["waiting_recorded_at"], "2026-10-07T06:44:53Z");
    let text = fixture.show_text(id);
    for expected in [
        "Still waiting: 9 admissible, 2 deferred and 5 excluded backlog task(s) were never started at the last pass (the owner answered 2026-10-07 06:44:53Z)",
        "Task ORB-101: context_lock_conflict blocked-by=ORB-900",
        "Task ORB-102: owner_hold (held for a red base: make ci-lint)",
        "Task ORB-103: unmet_dependency waits-on ORB-901",
        "Task ORB-104: host_os_mismatch (waits for a linux host (os:linux); the executor runs macos)",
        "Task ORB-105: crew_unavailable (crew antigravity cannot run on this host)",
        "Task ORB-106: native_os_required (Native OS requirement: criterion 2 needs native macos evidence, and the task's tags lack `os:macos`)",
        "Task ORB-107: browser_kit_missing (waits for a host with the browser kit (ui); run `orbit browser-kit install` on a host that runs workers to give it one; the executor reports no verified kit)",
        "idle: 20 backlog task(s) kept off this host for 4 consecutive passes (11 held on the owner, 4 footprint holds,",
        "1 needing a host with the browser kit",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
    }
    state
        .pull_drain
        .drain_last_pass
        .as_mut()
        .unwrap()
        .consecutive_idle_passes = 1;
    runtime.write_run_state(id, &state).unwrap();
    let text = fixture.show_text(id);
    assert!(text.contains("Task ORB-101:"));
    assert!(!text.contains("idle:"));
}
fn run_show_reports_deferred_tasks_beyond_the_listed_ones() {
    let fixture = Fixture::init();
    let id = "jrun-cli-pull-deferred-overflow";
    let now = chrono::Utc::now();
    let mut pass = pass_at(now);
    pass.deferred = (0..20)
        .map(|i| {
            waiting(
                &format!("ORB-{}", 200 + i),
                "owner_hold",
                &[],
                Some("held by the owner"),
            )
        })
        .collect();
    pass.deferred_total = 25;
    pass.waiting_by_reason = [("owner_hold".to_string(), 25)].into_iter().collect();
    pass.consecutive_idle_passes = 1;
    fixture.seed_pass(id, "workspace_pull_pipeline", pass);
    let shown = fixture.show_json(id);
    let pass = &shown["pipeline_state"]["drain_last_pass"];
    assert_eq!(pass["deferred_total"], 25);
    assert_eq!(pass["deferred"].as_array().unwrap().len(), 20);
    let text = fixture.show_text(id);
    for expected in [
        "Still waiting: 0 admissible, 25 deferred and 0 excluded backlog task(s) were never started at the last pass",
        "Task ORB-219: owner_hold",
        "... and 5 more deferred",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
    }
    assert!(!text.contains("Task ORB-220"));
}
fn run_show_names_the_pull_breaker() {
    let fixture = Fixture::init();
    let id = "jrun-cli-pull-breaker";
    let now = chrono::Utc::now();
    let (runtime, mut state) =
        fixture.seed_running(id, "workspace_pull_pipeline", serde_json::json!({}));
    let half_open_at = now + chrono::Duration::minutes(15);
    state.pull_drain.pull_breaker = Some(orbit_types::workflow::PullBreaker {
        failures: 3,
        failure_classes: vec!["candidate".into(), "task_input".into(), "untyped".into()],
        opened_at: now,
        half_open_at,
    });
    runtime.write_run_state(id, &state).unwrap();
    let shown = fixture.show_json(id);
    let breaker = &shown["pipeline_state"]["pull_breaker"];
    assert_eq!(breaker["failures"], 3);
    assert_eq!(breaker["failure_classes"][1], "task_input");
    let open = fixture.show_text(id);
    let half_open = format!(
        "half-opens at {} to admit one probe claim",
        half_open_at.format("%Y-%m-%d %H:%M:%SZ")
    );
    for expected in [
        "Breaker: open after 3 consecutive failed claims (candidate, task_input, untyped)",
        half_open.as_str(),
    ] {
        assert!(open.contains(expected), "missing {expected:?} in:\n{open}");
    }
    state.pull_drain.pull_breaker.as_mut().unwrap().half_open_at =
        now - chrono::Duration::minutes(1);
    runtime.write_run_state(id, &state).unwrap();
    let probing = fixture.show_text(id);
    assert!(probing.contains("half-open since"));
}
fn run_show_exposes_degraded_pull_pass_health() {
    let fixture = Fixture::init();
    let id = "jrun-cli-degraded-pull";
    let now = chrono::Utc::now();
    let mut pass = pass_at(now);
    pass.last_pass_error = Some("protocol_mismatch: caller revision 2; owner revision 1".into());
    pass.consecutive_pass_failures = 3;
    pass.degraded = true;
    let (runtime, mut state) = fixture.seed_pass(id, "workspace_pull_pipeline", pass);
    let shown = fixture.show_json(id);
    let pass = &shown["pipeline_state"]["drain_last_pass"];
    assert_eq!(pass["consecutive_pass_failures"], 3);
    assert_eq!(pass["degraded"], true);
    assert_eq!(
        pass["last_pass_error"],
        "protocol_mismatch: caller revision 2; owner revision 1"
    );
    let text = fixture.show_text(id);
    contains(&text, "consecutive_failures=3");
    contains(&text, "last_pass_error=protocol_mismatch:");
    contains(&text, "Drain degraded:");
    let pass = state.pull_drain.drain_last_pass.as_mut().unwrap();
    pass.last_pass_error_code = Some("protocol_skew".into());
    pass.last_pass_error =
        Some("protocol_skew: caller and owner request fingerprints differ".into());
    let skew_message = pass.last_pass_error.clone();
    runtime.write_run_state(id, &state).unwrap();
    fixture.db().execute("INSERT INTO job_run_steps (workspace_id, run_id, step_index, target_type, target_id, state, started_at, finished_at, error_code, error_message) VALUES (?1, ?2, 0, 'job', 'workspace_pull_pipeline', 'failed', ?3, ?3, 'protocol_skew', ?4)", params![fixture.workspace_id(), id, now.to_rfc3339(), skew_message]).unwrap();
    orbit_engine::RuntimeHost::finalize_job_run(
        &runtime,
        id,
        orbit_types::workflow::JobRunState::Failed,
        now,
        None,
    )
    .unwrap();
    let row = fixture.doctor_row("pull-protocol");
    assert_eq!(row["status"], "warning");
    assert!(row["message"].as_str().unwrap().contains("protocol_skew"));
    assert!(row["remediation"].as_str().unwrap().contains("restart"));
}
fn doctor_warns_when_the_replicas_gates_differ_from_the_owners() {
    let fixture = Fixture::init();
    let now = chrono::Utc::now().to_rfc3339();
    let input = serde_json::json!({"owner_validation_gates": {"required_validation_commands": ["make owner-only-check"], "baseline_commands": []}});
    fixture.db().execute("INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,input_json,scheduled_at,started_at,created_at,pid) VALUES ('jrun-cli-gate-drift',?1,'workspace_pull_pipeline',1,'running',?2,?3,?3,?3,?4)", params![fixture.workspace_id(), input.to_string(), now, std::process::id()]).unwrap();
    let row = fixture.doctor_row("pull-gates");
    assert_eq!(row["status"], "warning");
    assert!(
        row["message"]
            .as_str()
            .unwrap()
            .contains("but [`make owner-only-check`] on the owner")
    );
    assert!(
        row["remediation"]
            .as_str()
            .unwrap()
            .contains("`.orbit/config.toml`")
    );
}
#[cfg(unix)]
mod agent_invoke;
fn force_cancel_reports_unstopped_local_children_and_exits_one() {
    for as_json in [true, false] {
        let fixture = Fixture::init();
        let runtime = fixture.runtime();
        let drain = "jrun-cli-force-local";
        let failed = "jrun-cli-unstopped-child";
        let stopped = "jrun-cli-stopped-child";
        let now = chrono::Utc::now();
        for (id, job, state) in [
            (drain, "workspace_auto_pipeline", "pending"),
            (failed, "task_auto_pipeline", "running"),
            (stopped, "task_auto_pipeline", "pending"),
        ] {
            fixture.db().execute("INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,scheduled_at,started_at,created_at,pid) VALUES (?1,?2,?3,1,?4,?5,?5,?5,?6)", params![id, fixture.workspace_id(), job, state, now.to_rfc3339(), std::process::id()]).unwrap();
        }
        let mut state = fixture.write_state(
            &runtime,
            drain,
            "workspace_auto_pipeline",
            serde_json::json!({}),
        );
        for child in [failed, stopped] {
            dispatch(&mut state, child, "task_auto_pipeline", "dispatch", now);
        }
        runtime.write_run_state(drain, &state).unwrap();
        let mut command = fixture.orbit();
        command.args(["run", "cancel", drain, "--confirm", "--force"]);
        if as_json {
            command.arg("--json");
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(1));
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
fn a_surface_reservation_reaches_readiness_and_drain_run_show() {
    use orbit_core::{TaskComplexity, TaskPriority, TaskStatus};
    let fixture = Fixture::init();
    let runtime = fixture.runtime();
    for file in ["held.rs", "shared.rs"] {
        fs::write(fixture.work.join(file), "fixture\n").unwrap();
    }
    let add = |title: &str, status, priority, files: &[&str]| {
        let mut params = task_params(title.to_string(), format!("Fixture task: {title}"), status);
        params.complexity = TaskComplexity::Medium;
        params.priority = priority;
        params.acceptance_criteria = vec!["Fixture task is observable.".to_string()];
        params.plan = "Fixture plan.".to_string();
        params.context_files = files.iter().map(|file| format!("file:{file}")).collect();
        runtime.add_task(params).unwrap().id
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
    fixture.seed_running(drain, "workspace_auto_pipeline", serde_json::json!({}));
    let wave = run_det(
        &runtime,
        "classify_workspace_auto_tasks",
        serde_json::json!({"run_id": drain, "max_active_leaf_runs": 2}),
    );
    assert_eq!(wave["loose_task_ids"], serde_json::json!([]));
    let readiness = fixture.json(&["run", "readiness", "--json"]);
    let entry = readiness["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["task_id"] == withheld.as_str())
        .expect("withheld task in readiness");
    assert_eq!(entry["reason"], "surface_reserved");
    assert_eq!(entry["blocking_task_ids"], serde_json::json!([critical]));
    contains(
        &fixture.readiness_text(),
        &format!("{withheld}: waiting (surface_reserved) blocked-by={critical}"),
    );
    contains(
        &fixture.show_text(drain),
        &format!("Task {withheld}: surface_reserved blocked-by={critical}"),
    );
}
#[cfg(target_os = "linux")]
fn run_show_marks_an_agent_blocked_on_a_stopped_descendant() {
    use orbit_common::process::identity::{linux_process_stat, process_start_identity_token};
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    const BLOCKED: &str = "jrun-20261008-0100";
    const ALIVE: &str = "jrun-20261008-0200";
    struct AgentTree(std::process::Child);
    impl Drop for AgentTree {
        fn drop(&mut self) {
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
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            None => panic!("the agent's child never stopped"),
        }
    };
    let workspace_id = fixture.workspace_id();
    let db = fixture.db();
    let now = chrono::Utc::now().to_rfc3339();
    let mut events = Vec::new();
    for run_id in [BLOCKED, ALIVE] {
        db.execute("INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state, scheduled_at, started_at, created_at) VALUES (?1, ?2, 'task_pr_pipeline', 1, 'running', ?3, ?3, ?3)", params![run_id, workspace_id, now]).expect("seed running leaf");
        events.push((run_id, serde_json::json!({"event_id": format!("{run_id}-process"), "parent_event_id": format!("{run_id}-invocation"), "body_kind": "cli_invocation_process", "provider": "antigravity", "pid": agent_pid, "pid_start_time": process_start_identity_token(agent_pid)})));
    }
    events.push((BLOCKED, serde_json::json!({"event_id": format!("{BLOCKED}-stopped"), "parent_event_id": format!("{BLOCKED}-invocation"), "body_kind": "cli_invocation_stopped_descendant", "provider": "antigravity", "pid": child_pid, "pid_start_time": process_start_identity_token(child_pid), "command": "sh -c kill -STOP $$", "stopped_ms": 612_000, "ended": false, "error": "Operation not permitted (os error 1)"})));
    for (run_id, mut event) in events {
        event["ts"] = Value::String(now.clone());
        event["run_id"] = Value::String(run_id.to_string());
        event["step_id"] = Value::String("implement_one".to_string());
        db.execute("INSERT INTO v2_audit_events (workspace_id, event_id, source, schema_version, event_type, ts, run_id, agent_identity, parent_event_id, payload_json) VALUES (?1, ?2, 'v2_envelope', 1, 'activity.progress', ?3, ?4, 'test', ?5, ?6)", params![workspace_id, event["event_id"].as_str().unwrap(), now, run_id, event["parent_event_id"].as_str().unwrap(), event.to_string()]).expect("seed audit event");
    }
    let agent_line = |shown: &str| {
        shown
            .lines()
            .find(|line| line.starts_with("Agent:"))
            .unwrap_or_else(|| panic!("no Agent: line in {shown}"))
            .to_string()
    };
    let blocked = fixture.show_text(BLOCKED);
    assert!(agent_line(&blocked).contains(&format!(
        "pid={agent_pid} step=implement_one liveness=alive blocked=stopped-descendant "
    )));
    contains(
        &blocked,
        &format!(
            "  stopped descendant: pid={child_pid} command=`sh -c kill -STOP $$` stopped for at least 612s; the supervisor could not end it (Operation not permitted (os error 1)); still stopped"
        ),
    );
    let json = fixture.show_json(BLOCKED);
    let process = &json["provider_processes"][0];
    assert_eq!(process["liveness"], "alive");
    assert_eq!(process["blocked_on_stopped_descendant"]["pid"], child_pid);
    assert_eq!(
        process["blocked_on_stopped_descendant"]["still_stopped"],
        true
    );
    assert_eq!(process["stopped_descendants"][0]["ended"], false);
    let alive = fixture.show_text(ALIVE);
    let line = agent_line(&alive);
    assert!(line.contains("liveness=alive started_at=") && !line.contains("blocked="));
    assert!(
        fixture.show_json(ALIVE)["provider_processes"][0]["blocked_on_stopped_descendant"]
            .is_null()
    );
    drop(agent);
}
contract!(
    run_observation_contracts_0,
    successful_security_sweep_shows_filing_floor_source_and_excluded_alerts,
    shipped_security_job_uses_config_unless_run_input_overrides_it,
    readiness_bootstrap_preserves_stale_runs_and_held_reservations,
    readiness_reports_total_and_notifies_only_when_backlog_is_truncated
);
contract!(
    run_observation_contracts_1,
    job_replay_cli_reexecutes_deterministic_fixture_with_persisted_lineage,
    no_reconcile_run_reads_leave_stale_runs_and_reservations_unchanged,
    default_run_reads_still_reconcile_stale_runs,
    explicit_run_lookup_preserves_store_errors,
    run_events_select_resolved_steps_and_add_inherited_step_ids
);
#[test]
fn run_observation_contracts_2() {
    if !isolated_run_observation("run_observation::run_observation_contracts_2") {
        return;
    }
    run_rows(&[
        (
            stringify!(job_run_alias_produces_a_completed_trace_and_terminal_cancel_is_stable),
            job_run_alias_produces_a_completed_trace_and_terminal_cancel_is_stable as fn(),
        ),
        (
            stringify!(cancel_task_leaf_requeues_by_default_and_block_is_explicit),
            cancel_task_leaf_requeues_by_default_and_block_is_explicit as fn(),
        ),
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        (
            stringify!(cancelling_a_live_run_keeps_the_signalled_worker_exit_as_its_outcome),
            cancelling_a_live_run_keeps_the_signalled_worker_exit_as_its_outcome as fn(),
        ),
        (
            stringify!(replacement_drain_counts_and_reports_inherited_workers_until_they_finish),
            replacement_drain_counts_and_reports_inherited_workers_until_they_finish as fn(),
        ),
    ]);
}
contract!(
    run_observation_contracts_3,
    run_concurrency_updates_persisted_revision_and_refuses_stale_writes,
    run_extend_lengthens_a_live_drain_and_a_repeat_start_still_queues,
    a_drain_at_concurrency_twenty_runs_twenty_delivery_wrappers_at_once,
    run_concurrency_retunes_a_pull_drain_past_the_former_leaf_ceiling,
    readiness_uses_a_live_pull_drains_ceiling
);
contract!(
    run_observation_contracts_4,
    a_live_drains_recorded_throttle_reaches_readiness_run_show_and_ship,
    run_show_counts_local_deferred_tasks_once,
    run_show_names_the_tasks_a_pull_drains_owner_kept_off_this_host,
    run_show_reports_deferred_tasks_beyond_the_listed_ones
);
contract!(
    run_observation_contracts_5,
    run_show_names_the_pull_breaker,
    run_show_exposes_degraded_pull_pass_health,
    doctor_warns_when_the_replicas_gates_differ_from_the_owners,
    force_cancel_reports_unstopped_local_children_and_exits_one
);
fn warnings_hold_findings_reach_run_show_and_readiness() {
    use orbit_core::TaskStatus;
    let fixture = Fixture::init();
    fs::write(fixture.work.join("README.md"), "fixture\n").unwrap();
    for args in [
        &["config", "user.name", "Orbit Test"][..],
        &["config", "user.email", "orbit-test@example.com"][..],
        &["config", "commit.gpgsign", "false"][..],
        &["add", "README.md"][..],
        &["commit", "-m", "seed"][..],
    ] {
        git_repo::run(&fixture.work, args);
    }
    let runtime = fixture.runtime();
    let task = {
        let mut params = task_params(
            "Held proposal".into(),
            "A proposed change with a pilot warning.".into(),
            TaskStatus::Proposed,
        );
        params.task_type = None;
        params.acceptance_criteria = vec!["The README change is observable.".into()];
        params.plan = "Edit README.md.".into();
        params.context_files = vec!["file:README.md".into()];
        runtime.add_task(params)
    }
    .unwrap();
    let drain = "jrun-cli-warning-hold";
    let pilot = "jrun-cli-warning-pilot";
    let now = chrono::Utc::now();
    let db = fixture.db();
    for (run_id, job_id, input) in [
        (
            drain,
            "workspace_auto_pipeline",
            serde_json::json!({"approve_proposed": true}),
        ),
        (
            pilot,
            "task_pilot_pipeline",
            serde_json::json!({"task_ids": [task.id]}),
        ),
    ] {
        db.execute("INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,input_json,scheduled_at,started_at,created_at,pid) VALUES (?1,?2,?3,1,'running',?4,?5,?5,?5,?6)", params![run_id, fixture.workspace_id(), job_id, input.to_string(), now.to_rfc3339(), std::process::id()]).unwrap();
        fixture.write_state(&runtime, run_id, job_id, serde_json::json!({}));
    }
    let mut state = runtime.read_run_state(drain).unwrap().unwrap();
    dispatch(
        &mut state,
        pilot,
        "task_pilot_pipeline",
        "invoke_detached",
        now,
    );
    runtime.write_run_state(drain, &state).unwrap();
    let prepared = run_det(
        &runtime,
        "prepare_task_pilot",
        serde_json::json!({"task_ids": [task.id], "workspace_path": fixture.work, "base_branch": "main"}),
    );
    let first = "The operator should review the proposed approach.";
    let rest = "This second line stays in the audit only.";
    let surface = "The proposed change overlaps an open surface.";
    let applied = run_det(
        &runtime,
        "apply_task_pilot_results",
        serde_json::json!({"run_id": pilot, "workspace_path": fixture.work, "prepared": prepared, "promotion_authorized": true, "drain_promotion": {"run_id": drain}, "results": [{"partition_index": 0, "task_ids": [task.id], "tasks": [{"task_id": task.id, "context_files_before": ["file:README.md"], "context_files_after": ["file:README.md"], "disposition": "selectors", "recommended_crew": "fixture", "recommended_complexity": "low", "confidence": "high", "assessment_rationale": "README.md holds the change.", "validation_approach": "Inspect README.md.", "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [], "adr_conflicts": [], "utility_warnings": [format!("{first}\n{rest}")], "surface_warnings": [surface], "duplicate_of": null, "already_landed": null}]}]}),
    );
    assert_eq!(applied["status"], "succeeded");
    assert_eq!(applied["drain_approval"][0]["classification"], "warnings");
    assert_eq!(
        runtime.get_task(&task.id).unwrap().status,
        TaskStatus::Proposed
    );
    let selected = run_det(
        &runtime,
        "select_proposed_approvals",
        serde_json::json!({"approve_proposed": true, "run_id": drain}),
    );
    assert_eq!(selected["held"][0]["reason"], "warnings");
    let findings = serde_json::json!([{"kind": "utility_warnings", "text": format!("{first}\n{rest}")}, {"kind": "surface_warnings", "text": surface}]);
    assert_eq!(selected["held"][0]["findings"], findings);
    let shown = fixture.show_text(drain);
    contains(&shown, "Approved:");
    contains(&shown, &format!("    utility_warnings: {first}"));
    contains(&shown, &format!("    surface_warnings: {surface}"));
    assert!(
        shown.contains(&format!("{first}\n{rest}")),
        "run show keeps the full multiline finding: {shown}"
    );
    let readiness = fixture.json(&["run", "readiness", "--json"]);
    let held = readiness["approvals"]["held"]
        .as_array()
        .and_then(|held| held.iter().find(|e| e["task_id"] == task.id.as_str()))
        .unwrap_or_else(|| panic!("held task missing: {readiness:#}"));
    assert_eq!(held["reason"], "warnings");
    assert_eq!(held["findings"], findings);
}
contract!(
    run_observation_contracts_7,
    warnings_hold_findings_reach_run_show_and_readiness
);
#[test]
fn run_observation_contracts_6() {
    if !isolated_run_observation("run_observation::run_observation_contracts_6") {
        return;
    }
    tracing::info!(
        case = "a_surface_reservation_reaches_readiness_and_drain_run_show",
        "CLI contract row"
    );
    a_surface_reservation_reaches_readiness_and_drain_run_show();
    #[cfg(target_os = "linux")]
    {
        tracing::info!(
            case = "run_show_marks_an_agent_blocked_on_a_stopped_descendant",
            "CLI contract row"
        );
        run_show_marks_an_agent_blocked_on_a_stopped_descendant();
    }
}
