//! Deterministic observer faults against real children and a long-lived parent.
//! Store faults require the private supervision seam (unit admission criterion 2).

use std::process::Command;
use std::sync::mpsc;

use super::super::supervisor::{WorkerLaunchError, handoff_worker};

#[test]
fn a_closed_observer_channel_stops_and_reaps_the_spawned_child() {
    let (sender, receiver) = mpsc::sync_channel(1);
    drop(receiver);
    // Exec sleep directly: this fixture has no descendants to leak.
    let child = Command::new("sleep").arg("30").spawn().unwrap();
    let pid = child.id();
    let error = handoff_worker(sender, child).unwrap_err();
    assert!(
        matches!(error, WorkerLaunchError::Uncertain(_)),
        "a spawned worker must never be reported as not started"
    );
    // Reaping matters: an unreaped killed child still has a process identity.
    // SAFETY: signal 0 sends nothing and only probes whether `pid`, the child spawned
    // above, still exists; its arguments are plain integers, so no pointer precondition applies.
    assert_eq!(
        unsafe { libc::kill(pid as libc::pid_t, 0) },
        -1,
        "observer handoff failure must stop and reap its worker"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Child;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::Store;
use orbit_store::contracts::*;
use orbit_types::identity::Crew;
use orbit_types::workflow::{
    JobRun, JobRunStartOutcome, JobRunState, KnowledgeRunMetrics, PipelineState, RunStateUpdate,
};
use orbit_types::workspace::WorkspacePaths;

use super::super::super::wait::{sleep_until_worker_exit, worker_exits_seen};
use super::super::command::WorkerCommandConfig;
use super::super::supervisor::{PipelineRunHost, PipelineWorkerSupervisor};
use crate::runtime::event_bus::EventLog;

struct ReadFaultStore {
    inner: Arc<dyn JobRunStoreBackend>,
    reads: AtomicUsize,
    fail_at: usize,
    missing: bool,
    observed: mpsc::SyncSender<usize>,
    recover: Mutex<mpsc::Receiver<()>>,
}

impl JobRunStoreBackend for ReadFaultStore {
    fn get_job_run(&self, run_id: &str) -> Result<Option<JobRun>, OrbitError> {
        let read = self.reads.fetch_add(1, Ordering::SeqCst);
        if read == self.fail_at {
            self.observed.send(read).unwrap();
            return if self.missing {
                Ok(None)
            } else {
                Err(OrbitError::Store(
                    "injected startup observation fault".into(),
                ))
            };
        }
        if read == self.fail_at + 1 {
            self.observed.send(read).unwrap();
            self.recover
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(30))
                .unwrap();
        }
        self.inner.get_job_run(run_id)
    }

    fn workspace_id(&self) -> &str {
        self.inner.workspace_id()
    }

    fn with_execution_location(
        &self,
        location: Option<orbit_types::task::ExecutionLocation>,
    ) -> std::sync::Arc<dyn JobRunStoreBackend> {
        self.inner.with_execution_location(location)
    }

    fn list_job_runs(&self, job_id: &str) -> Result<Vec<JobRun>, OrbitError> {
        self.inner.list_job_runs(job_id)
    }

    fn list_job_runs_filtered(&self, query: &JobRunQuery) -> Result<Vec<JobRun>, OrbitError> {
        self.inner.list_job_runs_filtered(query)
    }

    fn count_job_runs_filtered(&self, query: &JobRunQuery) -> Result<u64, OrbitError> {
        self.inner.count_job_runs_filtered(query)
    }

    fn list_job_run_durations_filtered(&self, query: &JobRunQuery) -> Result<Vec<u64>, OrbitError> {
        self.inner.list_job_run_durations_filtered(query)
    }

    fn list_pending_or_running_job_runs(&self, job_id: &str) -> Result<Vec<JobRun>, OrbitError> {
        self.inner.list_pending_or_running_job_runs(job_id)
    }

    fn insert_job_run(
        &self,
        job_id: &str,
        attempt: u32,
        scheduled_at: DateTime<Utc>,
        input: Option<serde_json::Value>,
        retry_source_run_id: Option<String>,
    ) -> Result<JobRun, OrbitError> {
        self.inner
            .insert_job_run(job_id, attempt, scheduled_at, input, retry_source_run_id)
    }

    fn admit_child_job_run(
        &self,
        params: &ChildJobRunAdmissionParams,
    ) -> Result<ChildJobRunAdmissionOutcome, OrbitError> {
        self.inner.admit_child_job_run(params)
    }

    fn mark_job_run_running(
        &self,
        run_id: &str,
        started_at: DateTime<Utc>,
        pid: u32,
    ) -> Result<JobRunStartOutcome, OrbitError> {
        self.inner.mark_job_run_running(run_id, started_at, pid)
    }

    fn claim_pending_job_run_owner(&self, run_id: &str, pid: u32) -> Result<bool, OrbitError> {
        self.inner.claim_pending_job_run_owner(run_id, pid)
    }

    fn complete_job_run_step(
        &self,
        run_id: &str,
        params: &JobRunStepParams,
    ) -> Result<bool, OrbitError> {
        self.inner.complete_job_run_step(run_id, params)
    }

    fn record_job_run_knowledge_metrics(
        &self,
        run_id: &str,
        metrics: KnowledgeRunMetrics,
    ) -> Result<bool, OrbitError> {
        self.inner.record_job_run_knowledge_metrics(run_id, metrics)
    }

    fn record_job_run_crew(&self, run_id: &str, crew: &Crew) -> Result<bool, OrbitError> {
        self.inner.record_job_run_crew(run_id, crew)
    }

    fn finalize_job_run_with_outcome(
        &self,
        run_id: &str,
        state: JobRunState,
        finished_at: DateTime<Utc>,
        duration_ms: Option<u64>,
    ) -> Result<JobRunFinalization, OrbitError> {
        self.inner
            .finalize_job_run_with_outcome(run_id, state, finished_at, duration_ms)
    }

    fn cancel_pending_job_run(
        &self,
        run_id: &str,
        finished_at: DateTime<Utc>,
    ) -> Result<orbit_store::contracts::PendingJobRunCancellation, OrbitError> {
        self.inner.cancel_pending_job_run(run_id, finished_at)
    }

    fn repair_terminal_job_run_timing(
        &self,
        run_id: &str,
        finished_at: DateTime<Utc>,
        duration_ms: Option<u64>,
    ) -> Result<bool, OrbitError> {
        self.inner
            .repair_terminal_job_run_timing(run_id, finished_at, duration_ms)
    }

    fn list_all_pending_or_running_runs(&self) -> Result<Vec<JobRun>, OrbitError> {
        self.inner.list_all_pending_or_running_runs()
    }

    fn archive_job_run(&self, run_id: &str) -> Result<String, OrbitError> {
        self.inner.archive_job_run(run_id)
    }

    fn delete_job_run(&self, run_id: &str) -> Result<String, OrbitError> {
        self.inner.delete_job_run(run_id)
    }

    fn read_run_state(&self, run_id: &str) -> Result<Option<PipelineState>, OrbitError> {
        self.inner.read_run_state(run_id)
    }

    fn read_run_states(
        &self,
        run_ids: &[String],
    ) -> Result<HashMap<String, Option<PipelineState>>, OrbitError> {
        self.inner.read_run_states(run_ids)
    }

    fn write_run_state(&self, run_id: &str, state: &PipelineState) -> Result<(), OrbitError> {
        self.inner.write_run_state(run_id, state)
    }

    fn initialize_run_state(
        &self,
        run_id: &str,
        state: &PipelineState,
    ) -> Result<bool, OrbitError> {
        self.inner.initialize_run_state(run_id, state)
    }

    fn update_run_state(
        &self,
        run_id: &str,
        update: &mut dyn FnMut(JobRunState, &mut PipelineState) -> Result<(), OrbitError>,
    ) -> Result<RunStateUpdate, OrbitError> {
        self.inner.update_run_state(run_id, update)
    }
}

struct StoreHost(Arc<dyn JobRunStoreBackend>);

impl PipelineRunHost for StoreHost {
    fn reconciled_run(&self, run_id: &str) -> Result<JobRun, OrbitError> {
        Ok(self.0.get_job_run(run_id)?.unwrap())
    }

    fn providers_stopped(&self, _run_id: &str) -> bool {
        true
    }

    fn terminalize_run(
        &self,
        run_id: &str,
        state: JobRunState,
        finished_at: DateTime<Utc>,
    ) -> Result<bool, OrbitError> {
        Ok(matches!(
            self.0
                .finalize_job_run_with_outcome(run_id, state, finished_at, Some(0))?,
            JobRunFinalization::Finalized
        ))
    }
}

// Release both rendezvous on assertion unwinds, join the observer, and reap
// even the broken implementation's abandoned child. No signalling or descendants.
struct ObserverCleanup {
    pid: u32,
    child: Option<Child>,
    release: Option<TcpStream>,
    recover: mpsc::SyncSender<()>,
    observer: Option<JoinHandle<()>>,
}

impl Drop for ObserverCleanup {
    fn drop(&mut self) {
        if let Some(stream) = self.release.as_mut() {
            let _ = stream.write_all(b"exit");
        }
        let _ = self.recover.try_send(());
        if let Some(observer) = self.observer.take() {
            let _ = observer.join();
        }
        if let Some(child) = self.child.as_mut() {
            let _ = child.wait();
        }
        // ECHILD after a successful observer, otherwise reap its abandoned child.
        unsafe {
            libc::waitpid(self.pid as libc::pid_t, std::ptr::null_mut(), 0);
        }
    }
}

#[test]
fn store_read_faults_retain_supervision_and_reap_in_a_live_parent() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &store_read_faults_retain_supervision_and_reap_in_a_live_parent,
    )) {
        return;
    }

    // Startup errors, missing startup records, and a fresh-state read fault
    // after a claimed child exits exercise both sides of child.wait.
    for (fail_at, missing) in [(0, false), (0, true), (1, false)] {
        let root = tempfile::tempdir().unwrap();
        let paths = WorkspacePaths::new(
            root.path().to_path_buf(),
            root.path().join(".orbit"),
            root.path().join("global"),
        );
        let database = Store::open_in_memory().unwrap();
        let runs =
            orbit_store::compose::workspace_job_run_store(database.clone(), "observer-fixture");
        let run = runs
            .insert_job_run("observer-fixture", 1, Utc::now(), None, None)
            .unwrap();
        let (observed, reads) = mpsc::sync_channel(2);
        let (recover, recovery) = mpsc::sync_channel(1);
        let faults = Arc::new(ReadFaultStore {
            inner: runs.clone(),
            reads: AtomicUsize::new(0),
            fail_at,
            missing,
            observed,
            recover: Mutex::new(recovery),
        });
        let supervisor = PipelineWorkerSupervisor::new(
            faults,
            orbit_store::compose::audit_event_store_sqlite(database),
            paths.clone(),
            EventLog::default(),
            WorkerCommandConfig::for_paths(&paths),
            Arc::new(StoreHost(runs.clone())),
        );
        let log_path = root.path().join("worker.log");
        std::fs::File::create(&log_path).unwrap();
        let log_reader = std::fs::File::open(&log_path).unwrap();
        // The socket timeout is only a failure bound; release determines exit.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let child = Command::new("python3")
            .args([
                "-c",
                r#"
import socket, sys
s = socket.create_connection(("127.0.0.1", int(sys.argv[1])), timeout=30)
s.sendall(b"ready")
assert s.recv(4) == b"exit"
sys.exit(7)
"#,
                &listener.local_addr().unwrap().port().to_string(),
            ])
            .spawn()
            .unwrap();
        let pid = child.id();
        let mut cleanup = ObserverCleanup {
            pid,
            child: Some(child),
            release: None,
            recover,
            observer: None,
        };
        let (connected, connection) = mpsc::sync_channel(1);
        let accept = thread::spawn(move || {
            connected.send(listener.accept()).unwrap();
        });
        let (mut stream, _) = connection
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
            .unwrap();
        accept.join().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        let mut ready = [0; 5];
        stream.read_exact(&mut ready).unwrap();
        assert_eq!(&ready, b"ready");
        cleanup.release = Some(stream);
        if fail_at == 1 {
            runs.mark_job_run_running(&run.run_id, Utc::now(), pid)
                .unwrap();
        }
        let (finished, completion) = mpsc::sync_channel(1);
        let child = cleanup.child.take().unwrap();
        let run_id = run.run_id.clone();
        let exits = worker_exits_seen();
        cleanup.observer = Some(thread::spawn(move || {
            finished
                .send(supervisor.monitor_startup(&run_id, child, &log_path, log_reader, None))
                .unwrap();
        }));
        if fail_at == 1 {
            cleanup
                .release
                .as_mut()
                .unwrap()
                .write_all(b"exit")
                .unwrap();
        }
        assert_eq!(
            reads.recv_timeout(Duration::from_secs(30)).unwrap(),
            fail_at
        );
        assert_eq!(
            reads.recv_timeout(Duration::from_secs(30)).unwrap(),
            fail_at + 1,
            "a startup read fault must retain an observer that retries"
        );
        if fail_at == 0 {
            // SAFETY: signal 0 sends nothing and only checks that `pid` is still alive; the
            // child is blocked on the release byte this test has not sent, so it has not
            // exited, and its arguments are plain integers, so no pointer precondition applies.
            assert_eq!(
                unsafe { libc::kill(pid as libc::pid_t, 0) },
                0,
                "a read fault must not stop a potentially claimed worker"
            );
            assert_eq!(
                runs.get_job_run(&run.run_id).unwrap().unwrap().state,
                JobRunState::Pending
            );
            cleanup
                .release
                .as_mut()
                .unwrap()
                .write_all(b"exit")
                .unwrap();
        } else {
            assert_reaped(pid);
            assert_eq!(
                runs.get_job_run(&run.run_id).unwrap().unwrap().state,
                JobRunState::Running,
                "settlement needs fresh state even after the child is reaped"
            );
        }
        assert_eq!(
            worker_exits_seen(),
            exits,
            "a wait must not be woken before the observer reaps and settles its worker"
        );
        cleanup.recover.send(()).unwrap();
        completion
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
            .unwrap();
        cleanup.observer.take().unwrap().join().unwrap();
        // A wait that read the count before this exit, even one whose sleep
        // starts only now, returns at once; the interval only bounds failure.
        sleep_until_worker_exit(exits, Duration::from_secs(30));
        assert_eq!(
            worker_exits_seen(),
            exits + 1,
            "one reaped worker, one wake"
        );
        let settled = runs.get_job_run(&run.run_id).unwrap().unwrap();
        assert_eq!(
            settled.state,
            if fail_at == 0 {
                JobRunState::Interrupted
            } else {
                JobRunState::Failed
            }
        );
        assert_eq!(
            settled.steps.len(),
            1,
            "abnormal exit must retain its diagnostic"
        );
        assert_reaped(pid);
        // These assertions run while this isolated parent remains alive;
        // parent-process exit cannot accidentally provide the missing reaper.
    }
}

struct RescueHost {
    inner: Arc<dyn JobRunStoreBackend>,
    preserved: Arc<Mutex<Vec<String>>>,
}

impl PipelineRunHost for RescueHost {
    fn reconciled_run(&self, run_id: &str) -> Result<JobRun, OrbitError> {
        Ok(self.inner.get_job_run(run_id)?.unwrap())
    }

    fn providers_stopped(&self, _run_id: &str) -> bool {
        true
    }

    fn preserve_rescue_candidate(&self, run_id: &str) -> Option<String> {
        self.preserved.lock().unwrap().push(run_id.to_string());
        Some(format!(
            "rescue candidate kept at `refs/orbit/rescue/{run_id}` (commit `abc123`)"
        ))
    }

    fn terminalize_run(
        &self,
        run_id: &str,
        state: JobRunState,
        finished_at: DateTime<Utc>,
    ) -> Result<bool, OrbitError> {
        Ok(matches!(
            self.inner
                .finalize_job_run_with_outcome(run_id, state, finished_at, Some(0))?,
            orbit_store::contracts::JobRunFinalization::Finalized
        ))
    }
}

#[test]
fn a_sigterm_stopped_worker_keeps_its_rescue_candidate_before_terminalizing() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &a_sigterm_stopped_worker_keeps_its_rescue_candidate_before_terminalizing,
    )) {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let paths = WorkspacePaths::new(
        root.path().to_path_buf(),
        root.path().join(".orbit"),
        root.path().join("global"),
    );
    let database = Store::open_in_memory().unwrap();
    let runs = orbit_store::compose::workspace_job_run_store(database.clone(), "rescue-fixture");
    let run = runs
        .insert_job_run("rescue-fixture", 1, Utc::now(), None, None)
        .unwrap();
    let preserved = Arc::new(Mutex::new(Vec::new()));
    let supervisor = PipelineWorkerSupervisor::new(
        runs.clone(),
        orbit_store::compose::audit_event_store_sqlite(database),
        paths.clone(),
        EventLog::default(),
        WorkerCommandConfig::for_paths(&paths),
        Arc::new(RescueHost {
            inner: runs.clone(),
            preserved: preserved.clone(),
        }),
    );
    let log_path = root.path().join("worker.log");
    std::fs::File::create(&log_path).unwrap();
    let log_reader = std::fs::File::open(&log_path).unwrap();
    let child = Command::new("sleep").arg("30").spawn().unwrap();
    let pid = child.id();
    runs.mark_job_run_running(&run.run_id, Utc::now(), pid)
        .unwrap();
    // SAFETY: SIGTERM to the child this test just spawned; plain integers.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) }, 0);
    supervisor
        .monitor_startup(&run.run_id, child, &log_path, log_reader, None)
        .unwrap();
    assert_eq!(
        preserved.lock().unwrap().as_slice(),
        std::slice::from_ref(&run.run_id),
        "an externally stopped worker's candidate is kept once, before terminalizing"
    );
    let settled = runs.get_job_run(&run.run_id).unwrap().unwrap();
    assert_eq!(settled.state, JobRunState::Interrupted);
    let message = settled
        .steps
        .last()
        .and_then(|step| step.error_message.as_deref())
        .unwrap_or("");
    assert!(
        message.contains(&format!(
            "rescue candidate kept at `refs/orbit/rescue/{}`",
            run.run_id
        )),
        "{message}"
    );
}

fn assert_reaped(pid: u32) {
    // SAFETY: WNOHANG makes waitpid return at once instead of blocking, and a null status
    // pointer is allowed (waitpid(2) writes status only when it is non-null), so this only
    // queries whether `pid` is still our child.
    assert_eq!(
        unsafe { libc::waitpid(pid as libc::pid_t, std::ptr::null_mut(), libc::WNOHANG) },
        -1,
        "the observer must already have reaped its exact child"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
    // SAFETY: signal 0 sends nothing and only probes existence; the preceding waitpid found no
    // child to reap, so ESRCH is the expected result, and its arguments are plain integers.
    assert_eq!(
        unsafe { libc::kill(pid as libc::pid_t, 0) },
        -1,
        "no live or zombie worker may remain in the long-lived parent"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}
