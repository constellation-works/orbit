//! Supervisor failure paths, exercised without an `OrbitRuntime`.
//!
//! The fixture builds a supervisor from a temporary SQLite store, a recording
//! [`PipelineRunHost`], and an event log — which is the point of the type: a
//! worker that dies during startup, or one killed by an outstanding
//! cancellation, can be settled and asserted without composing a runtime.

use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::Store;
use orbit_store::compose::{audit_event_store_sqlite, workspace_job_run_store};
use orbit_store::contracts::{AuditEventFilter, AuditEventStoreBackend, JobRunStoreBackend};
use orbit_types::record::OrbitEvent;
use orbit_types::telemetry::{AuditEvent, AuditEventStatus};
use orbit_types::workflow::{JobRun, JobRunState};
use orbit_types::workspace::WorkspacePaths;
use tempfile::TempDir;

use crate::application::job::pipeline::worker::command::WorkerCommandConfig;
use crate::application::job::pipeline::worker::record::{self, PipelineAuditRow};
use crate::application::job::pipeline::worker::supervisor::{
    PipelineRunHost, PipelineWorkerSupervisor,
};
use crate::runtime::event_bus::EventLog;

#[cfg(target_os = "linux")]
use crate::application::job::pipeline::worker::scope::WorkerScopeCgroup;
#[cfg(unix)]
use crate::application::job::run::{CANCELLATION_REQUEST_AUDIT, CANCELLATION_WORKER_EXIT_AUDIT};

const WORKSPACE_ID: &str = "supervisor-fixture";

/// A terminalization the supervisor asked its host to perform.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Terminalization {
    run_id: String,
    state: JobRunState,
}

/// Records the run-lifecycle calls the supervisor delegates, and applies them
/// to the same store the supervisor reads, so later reads see the outcome.
struct RecordingHost {
    runs: Arc<dyn JobRunStoreBackend>,
    terminalizations: Mutex<Vec<Terminalization>>,
}

impl RecordingHost {
    fn terminalizations(&self) -> Vec<Terminalization> {
        self.terminalizations
            .lock()
            .expect("host recordings are not poisoned")
            .clone()
    }
}

impl PipelineRunHost for RecordingHost {
    fn providers_stopped(&self, _run_id: &str) -> bool {
        true
    }

    fn reconciled_run(&self, run_id: &str) -> Result<JobRun, OrbitError> {
        self.runs
            .get_job_run(run_id)?
            .ok_or_else(|| OrbitError::Execution(format!("run '{run_id}' is missing")))
    }

    fn terminalize_run(
        &self,
        run_id: &str,
        state: JobRunState,
        finished_at: DateTime<Utc>,
    ) -> Result<bool, OrbitError> {
        self.terminalizations
            .lock()
            .expect("host recordings are not poisoned")
            .push(Terminalization {
                run_id: run_id.to_string(),
                state,
            });
        self.runs.finalize_job_run(run_id, state, finished_at, None)
    }
}

struct Fixture {
    _root: TempDir,
    supervisor: PipelineWorkerSupervisor,
    runs: Arc<dyn JobRunStoreBackend>,
    audit_events: Arc<dyn AuditEventStoreBackend>,
    host: Arc<RecordingHost>,
    event_log: EventLog,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().expect("tempdir");
        let store = Store::open(&root.path().join("orbit.db")).expect("open fixture store");
        let runs = workspace_job_run_store(store.clone(), WORKSPACE_ID);
        let audit_events = audit_event_store_sqlite(store);
        let paths = WorkspacePaths::new(
            root.path().join("repo"),
            root.path().join("repo/.orbit"),
            root.path().join("global"),
        );
        let event_log = EventLog::default();
        let host = Arc::new(RecordingHost {
            runs: Arc::clone(&runs),
            terminalizations: Mutex::new(Vec::new()),
        });
        let supervisor = PipelineWorkerSupervisor::new(
            Arc::clone(&runs),
            Arc::clone(&audit_events),
            paths.clone(),
            event_log.clone(),
            WorkerCommandConfig::for_paths(&paths),
            Arc::clone(&host) as Arc<dyn PipelineRunHost>,
        );
        Self {
            _root: root,
            supervisor,
            runs,
            audit_events,
            host,
            event_log,
        }
    }

    fn pending_run(&self, job_id: &str) -> JobRun {
        self.runs
            .insert_job_run(job_id, 1, Utc::now(), None, None)
            .expect("insert pending run")
    }

    fn audits(&self, run_id: &str) -> Vec<AuditEvent> {
        self.audit_events
            .list_audit_events(&AuditEventFilter {
                job_run_id: Some(run_id.to_string()),
                limit: 50,
                ..AuditEventFilter::default()
            })
            .expect("list fixture audits")
    }
}

#[test]
fn startup_failure_records_diagnostic_state_event_and_audit() {
    let fixture = Fixture::new();
    let run = fixture.pending_run("startup_failure_pipeline");

    fixture
        .supervisor
        .finalize_startup_failure(&run, "worker could not start", None, Some("test-actor"))
        .expect("finalize startup failure");

    assert_eq!(
        fixture.host.terminalizations(),
        vec![Terminalization {
            run_id: run.run_id.clone(),
            state: JobRunState::Interrupted,
        }]
    );

    let stored = fixture
        .runs
        .get_job_run(&run.run_id)
        .expect("read settled run")
        .expect("settled run exists");
    assert_eq!(stored.state, JobRunState::Interrupted);
    let diagnostic = stored.steps.last().expect("startup diagnostic step");
    assert_eq!(diagnostic.state, JobRunState::Interrupted);
    assert_eq!(
        diagnostic.error_message.as_deref(),
        Some("worker could not start")
    );

    let completions = fixture
        .event_log
        .snapshot()
        .into_iter()
        .filter_map(|event| match event {
            OrbitEvent::JobRunCompleted { run_id, state, .. } if run_id == run.run_id => {
                Some(state)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(completions, vec![JobRunState::Interrupted.to_string()]);

    let audit = fixture
        .audits(&run.run_id)
        .into_iter()
        .find(|audit| audit.tool_name.as_deref() == Some("pipeline.worker.startup"))
        .expect("startup failure audit");
    assert_eq!(audit.status, AuditEventStatus::Failure);
    assert_eq!(audit.host.as_deref(), Some("test-actor"));
    assert_eq!(
        audit.error_message.as_deref(),
        Some("worker could not start")
    );
}

/// A worker that claimed the run owns its own outcome: the startup path that
/// races with it must write nothing.
#[test]
fn startup_failure_leaves_a_claimed_run_to_its_worker() {
    let fixture = Fixture::new();
    let run = fixture.pending_run("claimed_startup_pipeline");
    fixture
        .runs
        .claim_pending_job_run_owner(&run.run_id, 424_242)
        .expect("claim run for a live worker");

    fixture
        .supervisor
        .finalize_startup_failure(&run, "worker could not start", None, None)
        .expect("finalize startup failure");

    assert!(fixture.host.terminalizations().is_empty());
    let stored = fixture
        .runs
        .get_job_run(&run.run_id)
        .expect("read claimed run")
        .expect("claimed run exists");
    assert_eq!(stored.state, JobRunState::Pending);
    assert!(stored.steps.is_empty());
    assert!(fixture.event_log.snapshot().is_empty());
    assert!(fixture.audits(&run.run_id).is_empty());
}

#[cfg(unix)]
#[test]
fn cancellation_exit_is_recorded_without_terminalizing_the_run() {
    let fixture = Fixture::new();
    let run = fixture.pending_run("cancelled_worker_pipeline");
    // The request the cancelling caller would have written; only its tool name
    // and `request_id` matter to the lookup under test.
    record::pipeline_audit(
        fixture.audit_events.as_ref(),
        Path::new("/repo"),
        PipelineAuditRow {
            tool_name: CANCELLATION_REQUEST_AUDIT,
            target_id: Some(&run.run_id),
            actor: Some("operator"),
            status: AuditEventStatus::Success,
            arguments: serde_json::json!({ "request_id": "cancel-1", "run_id": run.run_id }),
            error_message: None,
        },
    )
    .expect("record cancellation request");

    let recorded = fixture
        .supervisor
        .record_cancellation_exit(
            &run,
            libc::SIGTERM,
            "signal: 15 (SIGTERM)",
            Some("observer"),
        )
        .expect("record cancellation exit");

    assert!(recorded, "an outstanding request owns this TERM exit");
    assert!(fixture.host.terminalizations().is_empty());
    let audit = fixture
        .audits(&run.run_id)
        .into_iter()
        .find(|audit| audit.tool_name.as_deref() == Some(CANCELLATION_WORKER_EXIT_AUDIT))
        .expect("worker-exit cancellation audit");
    let arguments = audit
        .arguments_json
        .as_deref()
        .map(|raw| serde_json::from_str::<serde_json::Value>(raw).expect("audit arguments"))
        .expect("audit carries arguments");
    assert_eq!(arguments["request_id"], "cancel-1");
    assert_eq!(arguments["signal_name"], "SIGTERM");
    assert_eq!(arguments["exit_status"], "signal: 15 (SIGTERM)");
}

/// Without an outstanding request, a TERM exit is not a cancellation: the
/// observer must fall through to its ordinary worker-failure path.
#[cfg(unix)]
#[test]
fn unrequested_signal_exit_is_not_treated_as_a_cancellation() {
    let fixture = Fixture::new();
    let run = fixture.pending_run("signalled_worker_pipeline");

    let recorded = fixture
        .supervisor
        .record_cancellation_exit(&run, libc::SIGKILL, "signal: 9 (SIGKILL)", None)
        .expect("record cancellation exit");

    assert!(!recorded);
    assert!(fixture.audits(&run.run_id).is_empty());
}

/// An actual SIGTERM after claim must leave the same terminal vocabulary as
/// the stale-owner reconciler, even when the observer wins the race.
#[cfg(unix)]
#[test]
fn claimed_ship_worker_sigterm_observer_first_is_interrupted() {
    use std::process::Command;
    use std::time::{Duration, Instant};

    use crate::application::job::pipeline::worker::log::configure_pipeline_worker_stdio;
    use crate::application::job::run::WORKER_TERMINATED_ERROR_CODE;

    let fixture = Fixture::new();
    let run = fixture.pending_run("workspace_ship_pipeline");
    let logs = fixture._root.path().join("logs");
    let mut command = Command::new("sleep");
    command.arg("30");
    let worker_log =
        configure_pipeline_worker_stdio(&mut command, &logs, &run.run_id).expect("worker log");
    let pid = fixture
        .supervisor
        .spawn_process(&run.run_id, None, command, worker_log)
        .expect("spawn worker");
    fixture
        .runs
        .mark_job_run_running(&run.run_id, Utc::now(), pid)
        .expect("claim worker");
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) }, 0);

    let deadline = Instant::now() + Duration::from_secs(5);
    let stored = loop {
        let stored = fixture
            .runs
            .get_job_run(&run.run_id)
            .expect("read run")
            .expect("run");
        if stored.state.is_terminal() {
            break stored;
        }
        assert!(
            Instant::now() < deadline,
            "observer did not settle signalled worker"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(stored.state, JobRunState::Interrupted);
    assert!(stored.steps.iter().any(|step| {
        step.state == JobRunState::Interrupted
            && step.error_code.as_deref() == Some(WORKER_TERMINATED_ERROR_CODE)
    }));
}

/// `TasksMax` of every scope the live containment tests launch.
#[cfg(target_os = "linux")]
const LIVE_TASKS_MAX: u32 = 32;

/// A contained sibling with one descendant, alive until it is reaped.
#[cfg(target_os = "linux")]
const SIBLING_WORKER: [&str; 3] = ["sh", "-c", "sleep 120 & wait"];

/// A fork bomb that refuses to run outside a worker scope and holds its one
/// descendant until the test opens the gate file named by `$1`.
#[cfg(target_os = "linux")]
const GATED_FORK_BOMB: &str = r#"case "$(cat /proc/self/cgroup)" in */orbit-worker-*.scope) ;; *) exit 97 ;; esac
sleep 120 &
while [ ! -e "$1" ]; do sleep 0.05; done
while :; do sleep 120 & done"#;

/// The contained workers one live test launched, reaped on every exit path.
///
/// Launches are strict: an unavailable scope refuses the spawn, and any built
/// command that would not start `systemd-run --scope` under a fresh unit is
/// refused too, so no payload can fall back to this process's cgroup. Cleanup
/// only signals a process proven to belong to one of those units.
#[cfg(target_os = "linux")]
struct ContainedWorkers {
    command: WorkerCommandConfig,
    workspace: std::path::PathBuf,
    logs: std::path::PathBuf,
    owned: Vec<OwnedWorker>,
}

#[cfg(target_os = "linux")]
struct OwnedWorker {
    pid: u32,
    unit: String,
    scope: Option<WorkerScopeCgroup>,
}

#[cfg(target_os = "linux")]
impl ContainedWorkers {
    fn new(fixture: &Fixture) -> Self {
        use orbit_config::{MemoryLimit, MemoryUnit, WorkerContainmentSettings};

        use crate::application::job::pipeline::worker::scope::WorkerLimits;

        let workspace = fixture._root.path().join("repo");
        std::fs::create_dir_all(&workspace).expect("fixture workspace");
        let command = WorkerCommandConfig::for_paths(&WorkspacePaths::new(
            workspace.clone(),
            workspace.join(".orbit"),
            fixture._root.path().join("global"),
        ))
        .contained(WorkerLimits::from_settings(&WorkerContainmentSettings {
            enabled: true,
            strict: true,
            memory_high: MemoryLimit::Bytes {
                amount: 48,
                unit: Some(MemoryUnit::M),
            },
            memory_max: MemoryLimit::Bytes {
                amount: 64,
                unit: Some(MemoryUnit::M),
            },
            tasks_max: LIVE_TASKS_MAX,
        }))
        .strict_containment(true);
        Self {
            command,
            logs: workspace.join(".orbit/logs"),
            workspace,
            owned: Vec::new(),
        }
    }

    /// Launch `argv` as `run`'s worker in its own scope, and wait until it
    /// runs there.
    fn spawn(
        &mut self,
        supervisor: &PipelineWorkerSupervisor,
        run: &JobRun,
        argv: &[&str],
    ) -> Result<(u32, WorkerScopeCgroup), OrbitError> {
        use std::time::{Duration, Instant};

        use crate::application::job::pipeline::worker::command::worker_command_override;
        use crate::application::job::pipeline::worker::log::configure_pipeline_worker_stdio;

        worker_command_override::set(argv.iter().copied());
        let built = self.command.build(&self.workspace, &run.run_id);
        worker_command_override::clear();
        let mut worker = built?;
        let unit = contained_unit(&worker).ok_or_else(|| {
            OrbitError::Execution(format!(
                "refusing to launch {:?} outside a fresh worker scope",
                worker.get_program()
            ))
        })?;
        let log = configure_pipeline_worker_stdio(&mut worker, &self.logs, &run.run_id)?;
        let pid = supervisor.spawn_process(&run.run_id, None, worker, log)?;
        self.owned.push(OwnedWorker {
            pid,
            unit: unit.clone(),
            scope: None,
        });

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(scope) = scope_named(pid, &unit) {
                if let Some(worker) = self.owned.iter_mut().find(|worker| worker.pid == pid) {
                    worker.scope = Some(scope.clone());
                }
                return Ok((pid, scope));
            }
            assert!(
                Instant::now() < deadline,
                "worker {pid} never entered {unit}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// SIGKILL every process in the owned scopes, then wait until each worker
    /// was reaped by its observer and each scope is empty. Names what is left
    /// when that does not happen in time.
    fn reap(&mut self) -> Result<(), String> {
        use std::time::{Duration, Instant};

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let mut leftovers = Vec::new();
            for worker in &mut self.owned {
                // A worker killed before its scope was located may already
                // have created it; its descendants are only reachable there.
                if worker.scope.is_none() {
                    worker.scope = scope_named(worker.pid, &worker.unit);
                }
                let members = worker
                    .scope
                    .as_ref()
                    .map(|scope| scope_members(scope.directory()))
                    .unwrap_or_default();
                for pid in std::iter::once(worker.pid).chain(members.iter().copied()) {
                    kill_if_owned(pid, &worker.unit);
                }
                if !reaped(worker.pid, &worker.unit) {
                    leftovers.push(format!("worker {} is not reaped", worker.pid));
                }
                if !members.is_empty() {
                    leftovers.push(format!("{} still holds {members:?}", worker.unit));
                }
            }
            if leftovers.is_empty() {
                self.owned.clear();
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(leftovers.join("; "));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for ContainedWorkers {
    fn drop(&mut self) {
        let outcome = self.reap();
        // A second panic while unwinding would abort and hide the first
        // failure, so an unwinding test only logs what it could not reap.
        if let Err(leftovers) = &outcome
            && std::thread::panicking()
        {
            tracing::error!(%leftovers, "live containment fixture left processes behind");
            return;
        }
        outcome.expect("live containment fixture reaps every process it owns");
    }
}

/// The unit a built worker command launches in, or `None` unless it is
/// `systemd-run --scope` under a fresh worker unit.
#[cfg(target_os = "linux")]
fn contained_unit(command: &std::process::Command) -> Option<String> {
    if command.get_program() != "systemd-run" {
        return None;
    }
    let options = command
        .get_args()
        .map_while(|arg| arg.to_str().filter(|arg| *arg != "--"))
        .collect::<Vec<_>>();
    if !options.contains(&"--scope") {
        return None;
    }
    options
        .iter()
        .find_map(|option| option.strip_prefix("--unit="))
        .filter(|unit| unit.starts_with("orbit-worker-") && unit.ends_with(".scope"))
        .map(str::to_string)
}

/// `pid`'s worker scope, only when it is the unit this fixture launched.
#[cfg(target_os = "linux")]
fn scope_named(pid: u32, unit: &str) -> Option<WorkerScopeCgroup> {
    WorkerScopeCgroup::of_process(pid).filter(|scope| scope.directory().ends_with(unit))
}

/// Whether `pid` runs in `unit`, or is the `systemd-run` about to create it.
#[cfg(target_os = "linux")]
fn owned_by(pid: u32, unit: &str) -> bool {
    let launcher = format!("--unit={unit}");
    scope_named(pid, unit).is_some()
        || std::fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|argv| {
            argv.split(|byte| *byte == 0)
                .any(|arg| arg == launcher.as_bytes())
        })
}

/// SIGKILL `pid` only if it belongs to `unit`. The pidfd pins the process
/// before the ownership check, so a PID reaped and reused in between is
/// never signalled.
#[cfg(target_os = "linux")]
fn kill_if_owned(pid: u32, unit: &str) {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    let Ok(raw_pid) = libc::pid_t::try_from(pid) else {
        return;
    };
    // SAFETY: pidfd_open takes a PID and flags and returns a new descriptor.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, raw_pid, 0) };
    let Ok(fd) = libc::c_int::try_from(fd) else {
        return;
    };
    if fd < 0 {
        return;
    }
    // SAFETY: the kernel returned a new descriptor we now own.
    let pidfd = unsafe { OwnedFd::from_raw_fd(fd) };
    if owned_by(pid, unit) {
        // SAFETY: the pidfd is owned and open; no siginfo is passed.
        unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                pidfd.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            );
        }
    }
}

/// Whether `pid` from `unit` is gone: not an unreaped child of this process
/// and no longer a process of that unit.
#[cfg(target_os = "linux")]
fn reaped(pid: u32, unit: &str) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return true;
    };
    // `state ppid …` follow the parenthesised command name.
    let mut fields = stat
        .rsplit_once(')')
        .map(|(_, rest)| rest.split_whitespace())
        .into_iter()
        .flatten();
    let zombie_child = fields.next() == Some("Z")
        && fields.next().and_then(|ppid| ppid.parse::<u32>().ok()) == Some(std::process::id());
    !zombie_child && !owned_by(pid, unit)
}

/// The processes in a scope; none once its cgroup is collected.
#[cfg(target_os = "linux")]
fn scope_members(directory: &Path) -> Vec<u32> {
    std::fs::read_to_string(directory.join("cgroup.procs"))
        .map(|procs| {
            procs
                .lines()
                .filter_map(|pid| pid.trim().parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Without a reachable scope the fixture refuses to start anything, rather
/// than running its payload in this process's cgroup.
#[cfg(target_os = "linux")]
#[test]
fn contained_workers_refuse_to_launch_without_a_scope() {
    use crate::application::job::pipeline::worker::scope::TestScopeAvailability;

    let fixture = Fixture::new();
    let mut workers = ContainedWorkers::new(&fixture);
    let _unavailable = TestScopeAvailability::unavailable("no user manager in this fixture");
    let marker = fixture._root.path().join("payload-ran");
    let marker_arg = marker.to_str().expect("utf-8 marker path");
    let run = fixture.pending_run("refused_fork_bomb");

    let error = workers
        .spawn(
            &fixture.supervisor,
            &run,
            &["sh", "-c", r#"touch "$1""#, "sh", marker_arg],
        )
        .expect_err("an unavailable scope refuses the launch");

    assert!(
        matches!(error, OrbitError::WorkerContainmentUnavailable { .. }),
        "{error}"
    );
    assert!(workers.owned.is_empty(), "nothing was spawned to own");
    assert!(!marker.exists(), "the payload never ran");
    let stored = fixture
        .runs
        .get_job_run(&run.run_id)
        .expect("read refused run")
        .expect("refused run exists");
    assert_eq!(stored.state, JobRunState::Pending);
    assert_eq!(stored.pid, None);
}

/// A live fixture that fails before its stress payload is released still
/// leaves nothing behind: its shells, their descendants and the sibling are
/// killed and reaped while the panic unwinds, and the fork loop never starts.
///
/// Ignored by default because CI and sandboxes have no user bus. On a Linux
/// host with one: `cargo test -p orbit-core contained_ -- --ignored`.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "needs a reachable systemd user manager"]
fn contained_workers_are_reaped_when_the_fixture_fails_early() {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::time::{Duration, Instant};

    let fixture = Fixture::new();
    let gate = fixture._root.path().join("never-opened");
    let gate_arg = gate.to_str().expect("utf-8 gate path");
    let mut owned = Vec::new();

    let failure = catch_unwind(AssertUnwindSafe(|| {
        let mut workers = ContainedWorkers::new(&fixture);
        let sibling = fixture.pending_run("reaped_sibling");
        workers
            .spawn(&fixture.supervisor, &sibling, &SIBLING_WORKER)
            .expect("spawn contained sibling");
        let bomb = fixture.pending_run("reaped_gated_fork_bomb");
        let (_, bomb_scope) = workers
            .spawn(
                &fixture.supervisor,
                &bomb,
                &["sh", "-c", GATED_FORK_BOMB, "sh", gate_arg],
            )
            .expect("spawn gated fork bomb");
        // Each shell is up with its descendant before the failure.
        let deadline = Instant::now() + Duration::from_secs(10);
        for worker in &workers.owned {
            let scope = worker.scope.as_ref().expect("located scope");
            while scope_members(scope.directory()).len() < 2 {
                assert!(Instant::now() < deadline, "{} never forked", worker.unit);
                std::thread::sleep(Duration::from_millis(20));
            }
            owned.push((
                worker.pid,
                worker.unit.clone(),
                scope.directory().to_path_buf(),
                scope_members(scope.directory()),
            ));
        }
        assert_eq!(bomb_scope.limit_breach(), None, "the gate held the loop");
        panic!("controlled failure before the fork bomb's gate opens");
    }));

    assert!(failure.is_err(), "the controlled failure unwound");
    assert!(!gate.exists());
    assert_eq!(owned.len(), 2, "sibling and fork bomb were both running");
    for (pid, unit, directory, members) in owned {
        assert!(members.contains(&pid), "{unit} held its worker");
        assert!(members.len() >= 2, "{unit} held a descendant: {members:?}");
        for member in members {
            assert!(reaped(member, &unit), "{member} of {unit} survived");
        }
        assert!(scope_members(&directory).is_empty(), "{unit} is empty");
    }
}

/// [ORB-12903] Live containment against the host's systemd user manager: a
/// worker that forks without bound under a tight `TasksMax` settles its own
/// run with `worker_resource_limit`, while a sibling worker in its own scope
/// and this parent process keep running. The loop only starts once its scope
/// and limits are verified, and every process is reaped however it ends.
///
/// Ignored by default because CI and sandboxes have no user bus. On a Linux
/// host with one: `cargo test -p orbit-core contained_ -- --ignored`.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "needs a reachable systemd user manager"]
fn contained_fork_bomb_fails_its_own_run_while_a_sibling_survives() {
    use std::time::{Duration, Instant};

    use crate::application::job::pipeline::worker::scope::WORKER_RESOURCE_LIMIT_ERROR_CODE;

    let fixture = Fixture::new();
    let mut workers = ContainedWorkers::new(&fixture);
    let gate = fixture._root.path().join("fork-bomb-gate");
    let gate_arg = gate.to_str().expect("utf-8 gate path");

    let sibling = fixture.pending_run("contained_sibling");
    let (sibling_pid, sibling_scope) = workers
        .spawn(&fixture.supervisor, &sibling, &SIBLING_WORKER)
        .expect("spawn contained sibling");

    let bomb = fixture.pending_run("contained_fork_bomb");
    let (_, bomb_scope) = workers
        .spawn(
            &fixture.supervisor,
            &bomb,
            &["sh", "-c", GATED_FORK_BOMB, "sh", gate_arg],
        )
        .expect("spawn gated fork bomb");
    assert_ne!(bomb_scope, sibling_scope, "each run gets its own scope");
    let limit = |name: &str| {
        std::fs::read_to_string(bomb_scope.directory().join(name))
            .expect("scope limit file")
            .trim()
            .to_string()
    };
    assert_eq!(limit("pids.max"), LIVE_TASKS_MAX.to_string());
    assert_eq!(limit("memory.max"), (64 * 1024 * 1024).to_string());
    std::fs::write(&gate, "").expect("release the verified fork bomb");

    let deadline = Instant::now() + Duration::from_secs(30);
    let settled = loop {
        let run = fixture
            .runs
            .get_job_run(&bomb.run_id)
            .expect("read bomb run")
            .expect("bomb run exists");
        if run.state.is_terminal() {
            break run;
        }
        assert!(
            Instant::now() < deadline,
            "the fork bomb's run never settled"
        );
        std::thread::sleep(Duration::from_millis(100));
    };

    let diagnostic = settled.steps.last().expect("bomb diagnostic step");
    assert_eq!(
        diagnostic.error_code.as_deref(),
        Some(WORKER_RESOURCE_LIMIT_ERROR_CODE),
        "{:?}",
        diagnostic.error_message
    );
    assert!(
        diagnostic
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("task limit"))
    );
    assert!(
        !reaped(sibling_pid, &workers.owned[0].unit),
        "the sibling worker outlives the bomb's run"
    );
    assert_eq!(sibling_scope.limit_breach(), None);
    let sibling_run = fixture
        .runs
        .get_job_run(&sibling.run_id)
        .expect("read sibling run")
        .expect("sibling run exists");
    assert!(!sibling_run.state.is_terminal());
    workers
        .reap()
        .expect("the bomb's leftovers and the sibling are reaped");
}

/// Exercise the production host (including reservation cleanup) in a child
/// with no inherited managed-run authority.
#[cfg(unix)]
#[test]
fn unexpected_exit_retains_reservations_until_provider_evidence_closes() {
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use orbit_common::process::identity::process_start_identity_token;
    use orbit_store::{TaskReservationReserveParams, V2AuditEventInsertParams};

    use crate::OrbitRuntime;
    use crate::application::job::pipeline::worker::log::configure_pipeline_worker_stdio;

    const CHILD: &str = "ORBIT_TEST_SUPERVISOR_PROVIDER_CHILD";
    const NAME: &str = concat!(
        module_path!(),
        "::unexpected_exit_retains_reservations_until_provider_evidence_closes"
    );
    if std::env::var(CHILD).ok().as_deref() != Some(NAME) {
        let home = TempDir::new().expect("isolated home");
        let mut child = Command::new(std::env::current_exe().expect("test binary"));
        orbit_common::test_env::clear_inherited_authority(|key| {
            child.env_remove(key);
        });
        let output = child
            .args([
                "--exact",
                NAME.strip_prefix("orbit_core::").unwrap_or(NAME),
                "--nocapture",
            ])
            .env(CHILD, NAME)
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .output()
            .expect("isolated supervisor test");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains("1 passed"),
            "child must execute regression: {stdout}"
        );
        return;
    }

    struct ObservedHost {
        runtime: OrbitRuntime,
        probes: AtomicUsize,
    }
    impl PipelineRunHost for ObservedHost {
        fn providers_stopped(&self, run_id: &str) -> bool {
            let stopped = PipelineRunHost::providers_stopped(&self.runtime, run_id);
            self.probes.fetch_add(1, Ordering::SeqCst);
            stopped
        }
        fn reconciled_run(&self, run_id: &str) -> Result<JobRun, OrbitError> {
            PipelineRunHost::reconciled_run(&self.runtime, run_id)
        }
        fn terminalize_run(
            &self,
            run_id: &str,
            state: JobRunState,
            at: DateTime<Utc>,
        ) -> Result<bool, OrbitError> {
            PipelineRunHost::terminalize_run(&self.runtime, run_id, state, at)
        }
    }

    // Both a verifiable live identity and an absent identity must defer.
    for known_identity in [true, false] {
        let root = TempDir::new().expect("fixture root");
        let global = root.path().join("global");
        let orbit_dir = root.path().join("repo/.orbit");
        std::fs::create_dir_all(&global).expect("global root");
        std::fs::create_dir_all(&orbit_dir).expect("workspace root");
        let runtime = OrbitRuntime::from_roots(&global, &orbit_dir).expect("runtime");
        let runs = Arc::clone(&runtime.stores().job_run);
        let run = runs
            .insert_job_run("provider_survivor", 1, Utc::now(), None, None)
            .expect("run");
        let host = Arc::new(ObservedHost {
            runtime: runtime.clone(),
            probes: AtomicUsize::new(0),
        });
        let events = EventLog::default();
        let supervisor = PipelineWorkerSupervisor::new(
            Arc::clone(&runs),
            Arc::clone(&runtime.stores().audit_event),
            runtime.paths().clone(),
            events.clone(),
            WorkerCommandConfig::for_paths(runtime.paths()),
            host.clone(),
        );
        runtime
            .stores()
            .task_reservations()
            .reserve_task_reservation(TaskReservationReserveParams {
                workspace_orbit_dir: orbit_dir.to_string_lossy().into_owned(),
                workspace_id: Some(runtime.workspace_id().expect("workspace id")),
                task_ids: Vec::new(),
                requested_files: vec!["file:src/provider.rs".to_string()],
                actor: "test".to_string(),
                ttl_seconds: 3600,
                owner_run_id: Some(run.run_id.clone()),
                owner_metadata_json: None,
            })
            .expect("reservation");
        let reserved = || {
            runtime
                .stores()
                .task_reservations()
                .list_active_task_reservations(
                    &orbit_dir.to_string_lossy(),
                    Some(&runtime.workspace_id().expect("workspace id")),
                )
                .expect("reservations")
                .reservations
                .iter()
                .any(|r| r.owner_run_id.as_deref() == Some(&run.run_id))
        };
        // This process survives the worker and has a real stable identity.
        let mut provider = Command::new("sleep").arg("30").spawn().expect("provider");
        let provider_pid = provider.id();
        let token =
            known_identity.then(|| process_start_identity_token(provider.id()).expect("identity"));
        let write_event = |finished: bool| {
            let id = if finished {
                "provider-finished"
            } else {
                "provider-spawn"
            };
            let payload = serde_json::json!({
                "event_id": id, "ts": Utc::now().to_rfc3339(), "run_id": run.run_id,
                "parent_event_id": "invocation", "step_id": "agent_implement", "provider": "codex",
                "body_kind": if finished { "cli_invocation_finished" } else { "cli_invocation_process" },
                "pid": provider_pid, "pid_start_time": token, "exit_code": 0, "timed_out": false,
            });
            runtime
                .insert_v2_audit_event(&V2AuditEventInsertParams {
                    workspace_id: runtime.workspace_id().expect("workspace"),
                    event_id: id.to_string(),
                    source: "v2_envelope".to_string(),
                    schema_version: 1,
                    event_type: "activity.progress".to_string(),
                    ts: Utc::now(),
                    run_id: run.run_id.clone(),
                    agent_identity: "test".to_string(),
                    parent_event_id: Some("invocation".to_string()),
                    workspace_path: None,
                    payload_json: payload.to_string(),
                })
                .expect("provider evidence");
        };
        write_event(false);
        let mut worker = Command::new("sleep");
        worker.arg("30");
        let log =
            configure_pipeline_worker_stdio(&mut worker, &root.path().join("logs"), &run.run_id)
                .expect("log");
        let pid = supervisor
            .spawn_process(&run.run_id, None, worker, log)
            .expect("worker");
        runs.mark_job_run_running(&run.run_id, Utc::now(), pid)
            .expect("claim");
        let signal = if known_identity {
            libc::SIGKILL
        } else {
            libc::SIGTERM
        };
        assert_eq!(unsafe { libc::kill(pid as i32, signal) }, 0);
        let deadline = Instant::now() + Duration::from_secs(10);
        while host.probes.load(Ordering::SeqCst) < 2 {
            assert!(
                Instant::now() < deadline,
                "observer must retry deferred exit"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            runs.get_job_run(&run.run_id).unwrap().unwrap().state,
            JobRunState::Running
        );
        assert!(reserved(), "surviving provider retains reservation");
        assert!(events.snapshot().is_empty(), "no premature completion");
        // Closing durable evidence allows the observer's original diagnostic
        // to settle exactly once, even for an unverifiable identity.
        if !known_identity {
            write_event(true);
        }
        provider.kill().expect("stop provider");
        provider.wait().expect("reap provider");
        loop {
            if events
                .snapshot()
                .iter()
                .any(|e| matches!(e, OrbitEvent::JobRunCompleted { .. }))
            {
                break;
            }
            assert!(Instant::now() < deadline, "closed evidence must settle");
            std::thread::sleep(Duration::from_millis(20));
        }
        let stored = runs.get_job_run(&run.run_id).unwrap().unwrap();
        assert_eq!(
            stored.state,
            if known_identity {
                JobRunState::Failed
            } else {
                JobRunState::Interrupted
            }
        );
        assert!(!reserved(), "settlement releases reservation");
        assert!(
            stored.finished_at.is_some() && stored.duration_ms.is_some(),
            "a run a dead worker left behind is stored complete, so later reads have nothing to repair"
        );
        let step = stored.steps.last().expect("original exit diagnostic");
        assert_eq!(
            step.error_code.as_deref(),
            (!known_identity).then_some(crate::application::job::run::WORKER_TERMINATED_ERROR_CODE)
        );
        assert!(
            step.error_message
                .as_deref()
                .unwrap()
                .contains(&format!("exited with status signal: {signal}"))
        );
        assert_eq!(
            events
                .snapshot()
                .iter()
                .filter(|e| matches!(e, OrbitEvent::JobRunCompleted { .. }))
                .count(),
            1
        );
    }
}
