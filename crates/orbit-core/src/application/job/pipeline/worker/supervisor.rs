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
//! configuration, and a [`PipelineRunHost`] for the run-lifecycle decisions it
//! must not own — so its failure paths can be exercised against a store alone.
//! [`OrbitRuntime`] keeps its worker methods as thin delegations.

use std::sync::Arc;

use chrono::DateTime;
use orbit_store::contracts::{AuditEventStoreBackend, JobRunStoreBackend};

use super::command::WorkerCommandConfig;
use super::record::{self, PipelineAuditRow};
use super::scope::{WORKER_RESOURCE_LIMIT_ERROR_CODE, WorkerScopeCgroup};
use super::*;
use crate::application::job::run::WORKER_TERMINATED_ERROR_CODE;
use crate::runtime::event_bus::EventLog;

#[cfg(unix)]
use crate::application::job::run::active_cancellation_request;

/// Whether a failed launch could already have executed the worker.
#[derive(Debug)]
pub(crate) enum WorkerLaunchError {
    /// No child was spawned; the queued run can safely be cancelled.
    NotStarted(OrbitError),
    /// A child existed; its run must retain launch intent for reconciliation.
    Uncertain(OrbitError),
}

impl From<OrbitError> for WorkerLaunchError {
    fn from(error: OrbitError) -> Self {
        Self::NotStarted(error)
    }
}

impl WorkerLaunchError {
    pub(crate) fn into_error(self) -> OrbitError {
        match self {
            Self::NotStarted(error) | Self::Uncertain(error) => error,
        }
    }
}

/// Transfer ownership to the observer, stopping and reaping an unobserved
/// child if the channel closed. Even a stopped child may already have run.
pub(super) fn handoff_worker(
    sender: mpsc::SyncSender<Child>,
    child: Child,
) -> Result<(), WorkerLaunchError> {
    sender.send(child).map_err(|error| {
        let cause =
            OrbitError::Execution(format!("hand pipeline worker to startup observer: {error}"));
        stop_unobserved_worker(error.0, cause)
    })
}

fn stop_unobserved_worker(mut child: Child, error: OrbitError) -> WorkerLaunchError {
    let stopped = child.kill().and_then(|()| child.wait().map(|_| ()));
    if let Err(stop_error) = stopped {
        tracing::error!(worker_pid = child.id(), %stop_error, "could not stop and reap unobserved pipeline worker");
    }
    WorkerLaunchError::Uncertain(error)
}

/// The run-lifecycle steps worker supervision delegates back to its host.
///
/// Terminalizing a run releases the run's task reservations and blocks the
/// tasks coupled to it, and a reconciled read repairs stale run records:
/// task-domain and reconciliation work that belongs to [`OrbitRuntime`], not
/// to a process supervisor. Provider liveness uses the same guard as orphan
/// reconciliation. Keeping these decisions behind this seam
/// is what lets the rest of supervision run without a runtime.
pub(crate) trait PipelineRunHost: Send + Sync {
    fn worker_bound(&self) -> bool {
        false
    }
    fn register_worker_process(&self, _pid: u32) -> Result<(), OrbitError> {
        Ok(())
    }

    /// The run as `orbit run show` reports it, after stale-run reconciliation.
    fn reconciled_run(&self, run_id: &str) -> Result<JobRun, OrbitError>;

    /// Whether durable provider evidence proves no provider remains active.
    fn providers_stopped(&self, run_id: &str) -> bool;

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
    fn worker_bound(&self) -> bool {
        self.worker_invocation().is_some()
    }
    fn register_worker_process(&self, pid: u32) -> Result<(), OrbitError> {
        OrbitRuntime::register_worker_process(self, pid)
    }

    fn reconciled_run(&self, run_id: &str) -> Result<JobRun, OrbitError> {
        self.show_job_run(run_id)
    }

    fn providers_stopped(&self, run_id: &str) -> bool {
        self.provider_evidence_allows_orphan_finalization(
            run_id,
            &orbit_common::process::identity::probe_process_liveness,
        )
    }

    fn terminalize_run(
        &self,
        run_id: &str,
        state: JobRunState,
        finished_at: DateTime<Utc>,
    ) -> Result<bool, OrbitError> {
        // A run a dead worker leaves behind still ran for a measurable time.
        // Storing no duration leaves the record "incomplete", which every
        // later read then tries to repair.
        let duration_ms = self.get_job_run_backend(run_id)?.map(|run| {
            let started_at = run.started_at.unwrap_or(run.scheduled_at);
            finished_at
                .signed_duration_since(started_at)
                .num_milliseconds()
                .max(0) as u64
        });
        self.finalize_job_run_with_reservation_cleanup(
            run_id,
            state,
            finished_at,
            duration_ms,
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
    pub(crate) fn spawn(&self, run_id: &str, actor: Option<&str>) -> Result<(), WorkerLaunchError> {
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
    ) -> Result<u32, WorkerLaunchError> {
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

        if self.host.worker_bound() {
            command.env("ORBIT_WORKER_CONTEXT_REQUIRED", "1");
        }
        let child = command
            .spawn()
            .map_err(|error| OrbitError::Execution(format!("spawn pipeline worker: {error}")))?;
        let child_pid = child.id();
        if let Err(error) = self.host.register_worker_process(child_pid) {
            return Err(stop_unobserved_worker(child, error));
        }
        handoff_worker(sender, child)?;
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
        // [ORB-12903] The worker's own scope, located while the child is
        // alive: once every process in it is gone the cgroup goes with it.
        let mut scope = None;
        loop {
            let run = self
                .runs
                .get_job_run(run_id)?
                .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, run_id.to_string()))?;
            // Read after the run: a claim observed here was made by the exec'd
            // worker, which `systemd-run --scope` only starts once it is
            // inside its scope.
            if scope.is_none() {
                scope = WorkerScopeCgroup::of_process(child_pid);
            }
            if run.pid == Some(child_pid) && !claimed {
                log_best_effort(
                    "audit worker claim",
                    run_id,
                    self.record_audit(
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
                    ),
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
                    log_best_effort(
                        "audit duplicate worker",
                        run_id,
                        self.record_audit(
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
                        ),
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
                let exit = format!(
                    "pipeline worker for run '{run_id}' exited with status {status} {ownership} \
                     the persisted run from registered workspace '{}'; worker log: '{}'",
                    workspace.display(),
                    worker_log.display(),
                );
                let (error_code, message) =
                    match scope.as_ref().and_then(WorkerScopeCgroup::limit_breach) {
                        Some(breach) => (
                            Some(WORKER_RESOURCE_LIMIT_ERROR_CODE),
                            format!("{}; {exit}{output_detail}", breach.describe()),
                        ),
                        None => (
                            {
                                #[cfg(unix)]
                                {
                                    status
                                        .signal()
                                        .filter(|signal| *signal == libc::SIGTERM)
                                        .map(|_| WORKER_TERMINATED_ERROR_CODE)
                                }
                                #[cfg(not(unix))]
                                {
                                    None
                                }
                            },
                            format!(
                                "{exit}{output_detail}; verify workspace registration, worker root \
                             discovery, and action availability"
                            ),
                        ),
                    };
                while !self.finalize_exit_failure(&run, error_code, &message, actor)? {
                    // Reaping the leader does not prove its independently
                    // grouped providers stopped. Keep the original diagnostic
                    // while waiting; never kill additional processes here.
                    thread::sleep(Duration::from_secs(1));
                }
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
    /// called. Pending exits are interrupted startup; a running worker with a
    /// SIGTERM exit is interrupted like a dead owner found by reconciliation.
    /// Returns false while provider evidence remains open, so the observer can
    /// retry with the original diagnostic. A terminal or replaced owner ends
    /// observation without changing its outcome.
    fn finalize_exit_failure(
        &self,
        run: &JobRun,
        error_code: Option<&str>,
        message: &str,
        actor: Option<&str>,
    ) -> Result<bool, OrbitError> {
        let current = self
            .runs
            .get_job_run(&run.run_id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, run.run_id.clone()))?;
        if current.pid != run.pid
            || current.pid_start_time != run.pid_start_time
            || current.state.is_terminal()
        {
            return Ok(true);
        }
        #[cfg(unix)]
        if active_cancellation_request(self.audit_events.as_ref(), &run.run_id)?.is_some() {
            return Ok(true);
        }
        if !self.host.providers_stopped(&run.run_id) {
            return Ok(false);
        }
        let (state, started_at, audit_name) = match current.state {
            JobRunState::Pending => (
                JobRunState::Interrupted,
                current.scheduled_at,
                "pipeline.worker.startup",
            ),
            JobRunState::Running => (
                if error_code == Some(WORKER_TERMINATED_ERROR_CODE) {
                    JobRunState::Interrupted
                } else {
                    JobRunState::Failed
                },
                current.started_at.unwrap_or(current.scheduled_at),
                "pipeline.worker.exit",
            ),
            _ => return Ok(true),
        };
        let finished_at = Utc::now();
        self.record_diagnostic_step(
            &current,
            started_at,
            finished_at,
            error_code,
            message,
            state,
        )?;
        self.terminalize(&current, state, finished_at)?;
        self.record_worker_failure_audit(audit_name, &current.run_id, message, actor)?;
        Ok(true)
    }

    /// Terminalize a run whose worker never started at all.
    pub(crate) fn finalize_startup_failure(
        &self,
        run: &JobRun,
        message: &str,
        error_code: Option<&str>,
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
            error_code,
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
        error_code: Option<&str>,
        message: &str,
        state: JobRunState,
    ) -> Result<(), OrbitError> {
        record::diagnostic_step(
            self.runs.as_ref(),
            run,
            started_at,
            finished_at,
            error_code,
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
