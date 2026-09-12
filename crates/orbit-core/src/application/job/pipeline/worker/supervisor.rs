//! Supervision of a detached pipeline worker process.
//!
//! A submitted run is executed by a child `orbit` process. Between spawning
//! that child and its exit, something on the parent side must watch it: record
//! when it claims the persisted run, reap it, and translate an exit that left
//! the run non-terminal into a diagnostic step, a terminal state, and an audit
//! row. That is one job, and [`PipelineWorkerSupervisor`] owns it.
//!
//! The supervisor is built from the handles that work needs — the run and
//! audit stores, workspace paths, the session event log, the worker command
//! configuration, and a [`PipelineRunHost`] for the two run-lifecycle steps it
//! must not own — so its failure paths can be exercised against a store alone.
//! [`OrbitRuntime`] keeps its worker methods as thin delegations.

use std::sync::Arc;

use chrono::DateTime;
use orbit_store::contracts::{AuditEventStoreBackend, JobRunStoreBackend};

use super::command::WorkerCommandConfig;
use super::record::{self, PipelineAuditRow};
use super::*;
use crate::runtime::event_bus::EventLog;

#[cfg(unix)]
use crate::application::job::run::active_cancellation_request;

/// The run-lifecycle steps worker supervision delegates back to its host.
///
/// Terminalizing a run releases the run's task reservations and blocks the
/// tasks coupled to it, and a reconciled read repairs stale run records:
/// task-domain and reconciliation work that belongs to [`OrbitRuntime`], not
/// to a process supervisor. Keeping exactly those two steps behind this seam
/// is what lets the rest of supervision run without a runtime.
pub(crate) trait PipelineRunHost: Send + Sync {
    /// The run as `orbit run show` reports it, after stale-run reconciliation.
    fn reconciled_run(&self, run_id: &str) -> Result<JobRun, OrbitError>;

    /// Terminalize `run_id`, releasing the task reservations it owns. Returns
    /// whether this call performed the terminal write.
    fn terminalize_run(
        &self,
        run_id: &str,
        state: JobRunState,
        finished_at: DateTime<Utc>,
    ) -> Result<bool, OrbitError>;
}

impl PipelineRunHost for OrbitRuntime {
    fn reconciled_run(&self, run_id: &str) -> Result<JobRun, OrbitError> {
        self.show_job_run(run_id)
    }

    fn terminalize_run(
        &self,
        run_id: &str,
        state: JobRunState,
        finished_at: DateTime<Utc>,
    ) -> Result<bool, OrbitError> {
        self.finalize_job_run_with_reservation_cleanup(
            run_id,
            state,
            finished_at,
            None,
            TaskReservationReleaseReason::RunTerminal,
        )
    }
}

/// Owns one workspace's detached worker processes: spawning them, watching
/// startup, and terminalizing a run whose worker died.
///
/// Cloneable and `'static` because the startup observer runs on its own
/// thread: it outlives the spawning call and must carry its own handles.
#[derive(Clone)]
pub(crate) struct PipelineWorkerSupervisor {
    runs: Arc<dyn JobRunStoreBackend>,
    audit_events: Arc<dyn AuditEventStoreBackend>,
    paths: WorkspacePaths,
    event_log: EventLog,
    command: WorkerCommandConfig,
    host: Arc<dyn PipelineRunHost>,
}

impl PipelineWorkerSupervisor {
    pub(crate) fn new(
        runs: Arc<dyn JobRunStoreBackend>,
        audit_events: Arc<dyn AuditEventStoreBackend>,
        paths: WorkspacePaths,
        event_log: EventLog,
        command: WorkerCommandConfig,
        host: Arc<dyn PipelineRunHost>,
    ) -> Self {
        Self {
            runs,
            audit_events,
            paths,
            event_log,
            command,
            host,
        }
    }

    /// The registered workspace every worker of this supervisor runs in, and
    /// the one named in its audit rows.
    fn workspace(&self) -> &Path {
        &self.paths.repo_root
    }

    /// Launch a detached worker for `run_id` and start watching its startup.
    pub(crate) fn spawn(&self, run_id: &str, actor: Option<&str>) -> Result<(), OrbitError> {
        let mut command = self.command.build(self.workspace(), run_id)?;
        let worker_log =
            configure_pipeline_worker_stdio(&mut command, &self.paths.logs_dir, run_id)?;
        self.spawn_process(run_id, actor, command, worker_log)
            .map(|_| ())
    }

    /// Spawn `command` as this run's worker, returning its PID.
    ///
    /// The startup observer is started *before* the process so every
    /// successfully spawned worker has a parent-side path that can terminalize
    /// a pre-claim exit.
    pub(crate) fn spawn_process(
        &self,
        run_id: &str,
        actor: Option<&str>,
        mut command: Command,
        worker_log: PipelineWorkerLog,
    ) -> Result<u32, OrbitError> {
        let PipelineWorkerLog {
            path: worker_log,
            reader: worker_log_reader,
        } = worker_log;

        #[cfg(unix)]
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let (sender, receiver) = mpsc::sync_channel::<Child>(1);
        let supervisor = self.clone();
        let run_id_for_observer = run_id.to_string();
        let actor_for_observer = actor.map(ToOwned::to_owned);
        let worker_log_for_observer = worker_log.clone();
        thread::Builder::new()
            .name(format!("pipeline-start-{run_id}"))
            .spawn(move || {
                let Ok(child) = receiver.recv() else {
                    return;
                };
                if let Err(error) = supervisor.monitor_startup(
                    &run_id_for_observer,
                    child,
                    &worker_log_for_observer,
                    worker_log_reader,
                    actor_for_observer.as_deref(),
                ) {
                    tracing::error!(
                        target: "orbit.core.job_run",
                        run_id = run_id_for_observer,
                        error = %error,
                        "failed to observe pipeline worker startup",
                    );
                }
            })
            .map_err(|error| {
                OrbitError::Execution(format!("spawn pipeline worker observer: {error}"))
            })?;

        let child = command
            .spawn()
            .map_err(|error| OrbitError::Execution(format!("spawn pipeline worker: {error}")))?;
        let child_pid = child.id();
        sender.send(child).map_err(|error| {
            OrbitError::Execution(format!("hand pipeline worker to startup observer: {error}"))
        })?;
        Ok(child_pid)
    }

    /// Watch `child` until it claims the run or exits, then settle the run.
    pub(crate) fn monitor_startup(
        &self,
        run_id: &str,
        mut child: Child,
        worker_log: &Path,
        mut worker_log_reader: File,
        actor: Option<&str>,
    ) -> Result<(), OrbitError> {
        let workspace = self.workspace();
        let child_pid = child.id();
        let mut claimed = false;
        loop {
            #[cfg(test)]
            worker_observer_read_counter::record_in(self.runs.as_ref(), run_id);
            let run = self
                .runs
                .get_job_run(run_id)?
                .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, run_id.to_string()))?;
            if run.pid == Some(child_pid) && !claimed {
                let _ = self.record_audit(
                    "pipeline.worker.claimed",
                    Some(run_id),
                    actor,
                    AuditEventStatus::Success,
                    json!({
                        "run_id": run_id,
                        "worker_pid": child_pid,
                        "owner_pid": child_pid,
                        "workspace": workspace,
                        "worker_log": worker_log,
                        "state": run.state.to_string(),
                    }),
                    None,
                );
                claimed = true;
            }

            // A persisted owner or non-pending state settles the only startup
            // question this observer owns. Waiting for the child avoids a
            // full run/step SQLite read every 25ms throughout normal work.
            let status = if run.pid.is_some() || run.state != JobRunState::Pending {
                Some(child.wait().map_err(|error| {
                    OrbitError::Execution(format!(
                        "wait for pipeline worker process for run '{run_id}': {error}"
                    ))
                })?)
            } else {
                child.try_wait().map_err(|error| {
                    OrbitError::Execution(format!(
                        "observe pipeline worker process for run '{run_id}': {error}"
                    ))
                })?
            };

            if let Some(status) = status {
                // The worker may have changed the run after the last startup
                // observation. Exit handling must use fresh state so duplicate
                // ownership, cancellation, and terminal outcomes stay
                // authoritative.
                #[cfg(test)]
                worker_observer_read_counter::record_in(self.runs.as_ref(), run_id);
                let run = self.runs.get_job_run(run_id)?.ok_or_else(|| {
                    OrbitError::not_found(NotFoundKind::JobRun, run_id.to_string())
                })?;
                let output = read_pipeline_worker_log_tail(&mut worker_log_reader);
                let output_detail = output
                    .as_deref()
                    .filter(|value| !value.is_empty())
                    .map(|value| format!("; worker output:\n{value}"))
                    .unwrap_or_default();
                // [ORB-11116] A second worker can lose the atomic Start race
                // and exit successfully while the incumbent's PID remains on
                // the run. Only this observer's exact child PID establishes
                // ownership; another non-null PID is a benign duplicate
                // delivery, not evidence that this child abandoned the run.
                if let Some(owner_pid) = run.pid.filter(|owner_pid| *owner_pid != child_pid) {
                    tracing::info!(
                        target: "orbit.core.job_run",
                        run_id,
                        worker_pid = child_pid,
                        owner_pid,
                        exit_status = %status,
                        "duplicate pipeline worker exited without owning the persisted run",
                    );
                    let _ = self.record_audit(
                        "pipeline.worker.duplicate",
                        Some(run_id),
                        actor,
                        AuditEventStatus::Success,
                        json!({
                            "run_id": run_id,
                            "worker_pid": child_pid,
                            "owner_pid": owner_pid,
                            "workspace": workspace,
                            "worker_log": worker_log,
                            "state": run.state.to_string(),
                            "exit_status": status.to_string(),
                        }),
                        None,
                    );
                    return Ok(());
                }
                #[cfg(unix)]
                if let Some(signal) = status
                    .signal()
                    .filter(|signal| matches!(*signal, libc::SIGTERM | libc::SIGKILL))
                    && self.record_cancellation_exit(&run, signal, &status.to_string(), actor)?
                {
                    // The cancelling caller owns terminalization after it has
                    // verified both the recorded leader and process group are
                    // gone. Reaping the worker proves only the leader exited;
                    // finalizing here could release reservations while a
                    // run-owned child remains alive.
                    return Ok(());
                }
                if run.state.is_terminal() {
                    return Ok(());
                }
                let ownership = if run.pid == Some(child_pid) {
                    "after claiming"
                } else {
                    "before claiming"
                };
                let message = format!(
                    "pipeline worker for run '{run_id}' exited with status {status} {ownership} \
                     the persisted run from registered workspace '{}'; worker log: \
                     '{}'{output_detail}; verify workspace registration, worker root discovery, \
                     and action availability",
                    workspace.display(),
                    worker_log.display(),
                );
                self.finalize_exit_failure(&run, &message, actor)?;
                return Ok(());
            }

            thread::sleep(Duration::from_millis(25));
        }
    }

    /// Record a TERM/KILL worker exit that belongs to an outstanding
    /// cancellation request, without terminalizing the run. The signalling
    /// caller performs the authoritative liveness verification and then
    /// finalizes `cancelled`; this observer only preserves the completion
    /// cause and suppresses the misleading generic worker-failure path.
    #[cfg(unix)]
    pub(crate) fn record_cancellation_exit(
        &self,
        run: &JobRun,
        signal: i32,
        exit_status: &str,
        actor: Option<&str>,
    ) -> Result<bool, OrbitError> {
        let Some(request_id) =
            active_cancellation_request(self.audit_events.as_ref(), &run.run_id)?
        else {
            return Ok(false);
        };
        self.record_audit(
            CANCELLATION_WORKER_EXIT_AUDIT,
            Some(&run.run_id),
            actor,
            AuditEventStatus::Success,
            json!({
                "request_id": request_id,
                "run_id": run.run_id,
                "owner_pid": run.pid,
                "signal": signal,
                "signal_name": worker_cancellation_signal_name(signal),
                "exit_status": exit_status,
                "observed_at": Utc::now().to_rfc3339(),
            }),
            None,
        )?;
        Ok(true)
    }

    /// Terminalize a worker process that exited while it still owned a
    /// non-terminal run. `try_wait` has already reaped the process when this is
    /// called. Pending exits are interrupted startup; a worker that reached
    /// running failed its claimed execution.
    fn finalize_exit_failure(
        &self,
        run: &JobRun,
        message: &str,
        actor: Option<&str>,
    ) -> Result<(), OrbitError> {
        let current = self
            .runs
            .get_job_run(&run.run_id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, run.run_id.clone()))?;
        let (state, started_at, audit_name) = match current.state {
            JobRunState::Pending => (
                JobRunState::Interrupted,
                current.scheduled_at,
                "pipeline.worker.startup",
            ),
            JobRunState::Running => (
                JobRunState::Failed,
                current.started_at.unwrap_or(current.scheduled_at),
                "pipeline.worker.exit",
            ),
            _ => return Ok(()),
        };
        let finished_at = Utc::now();
        self.record_diagnostic_step(&current, started_at, finished_at, message, state)?;
        self.terminalize(&current, state, finished_at)?;
        self.record_worker_failure_audit(audit_name, &current.run_id, message, actor)
    }

    /// Terminalize a run whose worker never started at all.
    pub(crate) fn finalize_startup_failure(
        &self,
        run: &JobRun,
        message: &str,
        actor: Option<&str>,
    ) -> Result<(), OrbitError> {
        let current = self.host.reconciled_run(&run.run_id)?;
        if current.state != JobRunState::Pending || current.pid.is_some() {
            return Ok(());
        }

        let finished_at = Utc::now();
        // Persist the diagnostic step before terminalizing the run: an observer
        // polling for a terminal state must never be able to see one without its
        // startup diagnostic already durable.
        self.record_diagnostic_step(
            run,
            run.scheduled_at,
            finished_at,
            message,
            JobRunState::Interrupted,
        )?;
        self.terminalize(run, JobRunState::Interrupted, finished_at)?;
        self.record_worker_failure_audit("pipeline.worker.startup", &run.run_id, message, actor)
    }

    /// Terminalize the run and announce the completion the write produced.
    /// A replayed terminalization changes nothing and emits nothing.
    fn terminalize(
        &self,
        run: &JobRun,
        state: JobRunState,
        finished_at: DateTime<Utc>,
    ) -> Result<(), OrbitError> {
        let changed = self.host.terminalize_run(&run.run_id, state, finished_at)?;
        if changed {
            self.event_log.append(OrbitEvent::JobRunCompleted {
                job_id: run.job_id.clone(),
                run_id: run.run_id.clone(),
                state: state.to_string(),
            });
        }
        Ok(())
    }

    fn record_diagnostic_step(
        &self,
        run: &JobRun,
        started_at: DateTime<Utc>,
        finished_at: DateTime<Utc>,
        message: &str,
        state: JobRunState,
    ) -> Result<(), OrbitError> {
        record::diagnostic_step(
            self.runs.as_ref(),
            run,
            started_at,
            finished_at,
            None,
            message,
            state,
        )
    }

    /// The audit row both failure paths write: same shape, different name.
    fn record_worker_failure_audit(
        &self,
        audit_name: &str,
        run_id: &str,
        message: &str,
        actor: Option<&str>,
    ) -> Result<(), OrbitError> {
        let worker_log = pipeline_worker_log_path(&self.paths.logs_dir, run_id)?;
        self.record_audit(
            audit_name,
            Some(run_id),
            actor,
            AuditEventStatus::Failure,
            json!({
                "run_id": run_id,
                "workspace": self.workspace(),
                "worker_log": worker_log,
            }),
            Some(message.to_string()),
        )
    }

    fn record_audit(
        &self,
        tool_name: &str,
        target_id: Option<&str>,
        actor: Option<&str>,
        status: AuditEventStatus,
        arguments: Value,
        error_message: Option<String>,
    ) -> Result<(), OrbitError> {
        record::pipeline_audit(
            self.audit_events.as_ref(),
            self.workspace(),
            PipelineAuditRow {
                tool_name,
                target_id,
                actor,
                status,
                arguments,
                error_message,
            },
        )
    }
}

#[cfg(unix)]
fn worker_cancellation_signal_name(signal: i32) -> &'static str {
    match signal {
        libc::SIGTERM => "SIGTERM",
        libc::SIGKILL => "SIGKILL",
        _ => "unknown",
    }
}

/// Counts the run-store reads the startup observer performs, so a test can
/// prove a claimed worker stops polling.
///
/// Keyed by the run store the observer reads through — shared by a runtime,
/// its clones, and the supervisors they build, while independent temporary
/// databases stay isolated — paired with the run ID.
#[cfg(test)]
pub(crate) mod worker_observer_read_counter {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};

    use orbit_store::contracts::JobRunStoreBackend;

    use crate::OrbitRuntime;

    type StoreRun = (usize, String);

    static COUNTS: LazyLock<Mutex<HashMap<StoreRun, usize>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    pub(crate) struct Counter {
        key: StoreRun,
    }

    fn key(runs: &dyn JobRunStoreBackend, run_id: &str) -> StoreRun {
        (
            runs as *const dyn JobRunStoreBackend as *const () as usize,
            run_id.to_string(),
        )
    }

    pub(crate) fn track(runtime: &OrbitRuntime, run_id: &str) -> Counter {
        let key = key(runtime.stores().jobs(), run_id);
        COUNTS
            .lock()
            .expect("test observer counters are not poisoned")
            .insert(key.clone(), 0);
        Counter { key }
    }

    pub(crate) fn record(runtime: &OrbitRuntime, run_id: &str) {
        record_in(runtime.stores().jobs(), run_id);
    }

    pub(crate) fn record_in(runs: &dyn JobRunStoreBackend, run_id: &str) {
        if let Some(count) = COUNTS
            .lock()
            .expect("test observer counters are not poisoned")
            .get_mut(&key(runs, run_id))
        {
            *count += 1;
        }
    }

    impl Counter {
        pub(crate) fn reads(&self) -> usize {
            *COUNTS
                .lock()
                .expect("test observer counters are not poisoned")
                .get(&self.key)
                .expect("tracked observer counter exists")
        }
    }

    impl Drop for Counter {
        fn drop(&mut self) {
            COUNTS
                .lock()
                .expect("test observer counters are not poisoned")
                .remove(&self.key);
        }
    }
}
