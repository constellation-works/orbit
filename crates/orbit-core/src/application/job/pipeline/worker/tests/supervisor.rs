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
