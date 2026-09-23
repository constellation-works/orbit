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
        .finalize_startup_failure(&run, "worker could not start", Some("test-actor"))
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
        .finalize_startup_failure(&run, "worker could not start", None)
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

/// [ORB-12903] Live containment against the host's systemd user manager: a
/// worker that forks without bound under a tight `TasksMax` settles its own
/// run with `worker_resource_limit`, while a sibling worker in its own scope
/// and this parent process keep running.
///
/// Ignored by default because CI and sandboxes have no user bus. On a Linux
/// host with one: `cargo test -p orbit-core contained_fork_bomb -- --ignored`.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "needs a reachable systemd user manager"]
fn contained_fork_bomb_fails_its_own_run_while_a_sibling_survives() {
    use std::time::{Duration, Instant};

    use orbit_config::WorkerContainmentSettings;

    use crate::application::job::pipeline::worker::command::worker_command_override;
    use crate::application::job::pipeline::worker::log::configure_pipeline_worker_stdio;
    use crate::application::job::pipeline::worker::scope::{
        WORKER_RESOURCE_LIMIT_ERROR_CODE, WorkerLimits, WorkerScopeCgroup,
    };

    const TASKS_MAX: u32 = 32;
    let fixture = Fixture::new();
    let workspace = fixture._root.path().join("repo");
    std::fs::create_dir_all(&workspace).expect("fixture workspace");
    let logs_dir = workspace.join(".orbit/logs");
    let command = WorkerCommandConfig::for_paths(&WorkspacePaths::new(
        workspace.clone(),
        workspace.join(".orbit"),
        fixture._root.path().join("global"),
    ))
    .contained(WorkerLimits::from_settings(&WorkerContainmentSettings {
        enabled: true,
        memory_high: "48M".to_string(),
        memory_max: "64M".to_string(),
        tasks_max: TASKS_MAX,
    }));
    let spawn = |run: &JobRun, argv: &[&str]| -> u32 {
        worker_command_override::set(argv.iter().copied());
        let mut worker = command
            .build(&workspace, &run.run_id)
            .expect("build contained worker");
        worker_command_override::clear();
        let log = configure_pipeline_worker_stdio(&mut worker, &logs_dir, &run.run_id)
            .expect("worker log");
        fixture
            .supervisor
            .spawn_process(&run.run_id, None, worker, log)
            .expect("spawn contained worker")
    };
    let scope_of = |pid: u32| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(scope) = WorkerScopeCgroup::of_process(pid) {
                return scope;
            }
            assert!(
                Instant::now() < deadline,
                "worker {pid} never entered a scope"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };

    let sibling = fixture.pending_run("contained_sibling");
    let sibling_pid = spawn(&sibling, &["sh", "-c", "sleep 8"]);
    let sibling_scope = scope_of(sibling_pid);

    let bomb = fixture.pending_run("contained_fork_bomb");
    let bomb_pid = spawn(&bomb, &["sh", "-c", "while :; do sleep 120 & done"]);
    let bomb_scope = scope_of(bomb_pid);
    assert_ne!(bomb_scope, sibling_scope, "each run gets its own scope");
    let limit = |name: &str| {
        std::fs::read_to_string(bomb_scope.directory().join(name))
            .expect("scope limit file")
            .trim()
            .to_string()
    };
    assert_eq!(limit("pids.max"), TASKS_MAX.to_string());
    assert_eq!(limit("memory.max"), (64 * 1024 * 1024).to_string());

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
    // The bomb's leftover children are in its session; reap the scope.
    unsafe {
        libc::kill(-(bomb_pid as i32), libc::SIGKILL);
    }

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
    assert_eq!(
        unsafe { libc::kill(sibling_pid as i32, 0) },
        0,
        "the sibling worker outlives the bomb's run"
    );
    assert_eq!(sibling_scope.limit_breach(), None);
    let sibling_run = fixture
        .runs
        .get_job_run(&sibling.run_id)
        .expect("read sibling run")
        .expect("sibling run exists");
    assert!(!sibling_run.state.is_terminal());
    unsafe {
        libc::kill(-(sibling_pid as i32), libc::SIGKILL);
    }
}
