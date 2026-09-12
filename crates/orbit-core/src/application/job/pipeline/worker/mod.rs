//! The worker side of a pipeline run, split by who owns which half.
//!
//! - `command` / `log` — how a worker process is launched and where its stdio
//!   lands.
//! - `supervisor` — the parent-side [`PipelineWorkerSupervisor`] that spawns a
//!   worker, watches its startup, and settles a run whose worker died.
//! - `record` — the store writes both sides share.
//!
//! What stays on [`OrbitRuntime`] here is the *child* process's own work:
//! executing the run it was handed, plus thin delegations to the supervisor
//! for callers that already hold a runtime.

use std::sync::Arc;

use super::*;
use command::*;
use log::*;
use supervisor::PipelineWorkerSupervisor;

use super::admission::pipeline_run_is_runnable;
use super::wait::PIPELINE_WAIT_MIN_POLL_SECONDS;

pub(super) mod command;
pub(super) mod log;
mod record;
pub(super) mod supervisor;

#[cfg(test)]
mod tests;

impl OrbitRuntime {
    pub fn execute_pipeline_run_worker(&self, run_id: &str) -> Result<(), OrbitError> {
        self.preflight_pipeline_worker_store()?;

        // [ORB-10070] Claim the queued run for this worker process so orphan
        // reconciliation can tell a pending run whose worker is alive and
        // polling for its admission slot apart from one whose worker died
        // (crash, SIGKILL, host reboot). Best-effort: the run may already be
        // running/terminal, and a claim failure must never block execution.
        if let Err(error) = self
            .stores()
            .jobs()
            .claim_pending_job_run_owner(run_id, std::process::id())
        {
            tracing::warn!(
                target: "orbit.core.job_run",
                run_id,
                error = %error,
                "pipeline worker could not claim its pending run; orphan \
                 detection falls back to the unclaimed-run grace window",
            );
        }
        loop {
            let run = self.show_job_run(run_id)?;
            match run.state {
                JobRunState::Pending => {}
                JobRunState::Running
                | JobRunState::Success
                | JobRunState::Failed
                | JobRunState::Timeout
                | JobRunState::Cancelled
                | JobRunState::Interrupted => return Ok(()),
                other => {
                    return Err(OrbitError::Execution(format!(
                        "pipeline worker cannot execute run '{}' from state '{}'",
                        run_id, other
                    )));
                }
            }

            let (yaml_path, spec) = self.resolve_run_definition(&run)?;
            if spec.state != JobScheduleState::Enabled {
                let _ = self.cancel_job_run(&run.run_id);
                return Err(OrbitError::InvalidInput(format!(
                    "job '{}' is disabled",
                    run.job_id
                )));
            }

            if let Err(error) = self.verify_routine_dispatch_workspace(&run) {
                self.record_routine_dispatch_workspace_mismatch(&run, &error);
                let _ = self.cancel_job_run(&run.run_id);
                return Err(error);
            }

            self.reconcile_stale_job_runs(Some(&run.job_id))?;
            let active_runs = self
                .stores()
                .jobs()
                .list_pending_or_running_job_runs(&run.job_id)?;
            if !pipeline_run_is_runnable(&active_runs, &run.run_id, spec.max_active_runs) {
                thread::sleep(Duration::from_secs(PIPELINE_WAIT_MIN_POLL_SECONDS));
                continue;
            }

            return self.execute_pipeline_run_now(&run, &yaml_path);
        }
    }
    /// [ORB-11998] A routine-dispatched run declares the `.orbit` directory of
    /// the workspace that owns it (see [`ROUTINE_DISPATCH_ORBIT_DIR_FIELD`]).
    /// Confirm this worker actually opened that same workspace before it runs
    /// any step. A mismatch — an ambient `ORBIT_ROOT`, an unregistered cwd, or
    /// any other workspace-routing failure — must fail the run visibly rather
    /// than silently execute (or vacuously succeed) against the wrong scope.
    /// A run with no declared field is not routine-dispatched and is
    /// unaffected.
    ///
    /// [ORB-12038] The refusal terminalizes the run as `cancelled`, not
    /// `failed`. It reuses [`Self::cancel_job_run`] unchanged — the same
    /// request/signal/completion audit trail, reservation release, and
    /// child-cascade settlement that every other cancellation gets — because
    /// nothing about that machinery is wrong here; only the missing
    /// diagnostic was. `failed` would read more accurately for a routing
    /// refusal than an operator action, but that relabeling is a wider
    /// contract change than this diagnostics fix and is deliberately left
    /// alone; see [`Self::record_routine_dispatch_workspace_mismatch`] for the
    /// diagnostic itself.
    fn verify_routine_dispatch_workspace(&self, run: &JobRun) -> Result<(), OrbitError> {
        let Some(declared) = run
            .input
            .as_ref()
            .and_then(|input| input.get(ROUTINE_DISPATCH_ORBIT_DIR_FIELD))
            .and_then(Value::as_str)
        else {
            return Ok(());
        };
        let declared_dir = Path::new(declared);
        let actual_dir = &self.paths().orbit_dir;
        if declared_dir == actual_dir.as_path() {
            return Ok(());
        }
        Err(OrbitError::WorkspaceError(format!(
            "run '{}' was dispatched for workspace '{}' but this worker resolved workspace '{}'; \
             refusing to execute against a mismatched workspace context",
            run.run_id,
            declared_dir.display(),
            actual_dir.display(),
        )))
    }
    /// [ORB-12038] Persist the guard's own message as a diagnostic step before
    /// [`Self::cancel_job_run`] terminalizes the run.
    ///
    /// The run is still `pending` here (`execute_pipeline_run_worker` has not
    /// reached `execute_pipeline_run_now`, so there is no `running` step to
    /// attach an error to), and cancellation itself records no error detail —
    /// it is written for an operator-requested stop, which carries no
    /// message. Without this, the guard's declared-vs-resolved diagnostic
    /// existed only in the worker process's own stderr and its
    /// `<run_id>.worker.log`, never on the run `orbit run show` displays.
    /// Best-effort: a failure to persist the diagnostic must not stop the
    /// run from being cancelled or the original error from propagating.
    fn record_routine_dispatch_workspace_mismatch(&self, run: &JobRun, error: &OrbitError) {
        let now = Utc::now();
        let _ = self.record_pipeline_diagnostic_step(
            run,
            run.scheduled_at,
            now,
            Some(ROUTINE_DISPATCH_WORKSPACE_MISMATCH_ERROR_CODE),
            &error.to_string(),
            JobRunState::Cancelled,
        );
    }
    /// Reopen the shared SQLite store before the worker claims a run.
    ///
    /// ADR-0287: another Orbit process may advance the host-global database
    /// while this runtime remains alive. Reopening here applies migrations
    /// supported by this binary or trips the downgrade guard before any agent
    /// work. Invocation persistence still reopens independently, but it can no
    /// longer be the first compatibility check after useful work completes.
    pub(crate) fn preflight_pipeline_worker_store(&self) -> Result<(), OrbitError> {
        self.ensure_persistence_ready()?;
        Ok(())
    }
    fn execute_pipeline_run_now(&self, run: &JobRun, yaml_path: &Path) -> Result<(), OrbitError> {
        let started_at = Utc::now();
        // [ORB-10965] The state read in `execute_pipeline_run_worker` and this
        // Start are separate transactions, so a second worker handed the same
        // queued run can arrive here having also seen `pending`. The store
        // arbitrates atomically; whoever does not win execution authority
        // yields here, before any agent work.
        let outcome = match self.stores().jobs().mark_job_run_running(
            &run.run_id,
            started_at,
            std::process::id(),
        ) {
            Ok(outcome) => outcome,
            Err(OrbitError::JobRunStartConflict(diagnostic)) => {
                return self.record_deduplicated_start(run, &diagnostic);
            }
            Err(error) => return Err(error),
        };
        match outcome {
            JobRunStartOutcome::Started => {}
            JobRunStartOutcome::AlreadyStarted => {
                return self.record_deduplicated_start(
                    run,
                    "this worker process had already started the run",
                );
            }
            JobRunStartOutcome::NotFound => return Ok(()),
        }
        let input = run
            .input
            .clone()
            .unwrap_or_else(|| Value::Object(Default::default()));
        // Once Start succeeds, every later error belongs to this run. Keep the
        // whole setup path inside the outcome finalized below so crew
        // validation, event persistence, and resume-state reads cannot escape
        // with a durable `running` projection.
        let outcome = (|| {
            self.record_run_crew_from_input(&run.run_id, &input)?;

            self.record_event(OrbitEvent::JobRunStarted {
                job_id: run.job_id.clone(),
                run_id: run.run_id.clone(),
                attempt: run.attempt,
            })?;

            // [ORB-10470] The run's own persisted checkpoints are the resume
            // cursor. A run seeded by `submit_resume_run` starts at the
            // source's first non-successful step; a run whose previous worker
            // died after checkpointing continues from where that worker
            // stopped. Reusing a checkpoint is therefore idempotent — the
            // successful steps are skipped, never re-dispatched.
            let resume = self.read_run_state(&run.run_id)?.filter(|state| {
                state
                    .step_states
                    .values()
                    .any(|step_state| *step_state == JobRunState::Success)
            });
            self.run_job_v2_from_yaml_with_run_id_and_resume(
                yaml_path,
                input.clone(),
                Some(run.run_id.clone()),
                run.retry_source_run_id.clone(),
                resume.as_ref(),
            )
        })();
        let finished_at = Utc::now();
        self.finalize_v2_pipeline_run(
            run,
            &input,
            started_at,
            finished_at,
            outcome.as_ref(),
            V2RunFinalizationOptions::DETACHED_WORKER,
        )?;
        outcome.map(|_| ())
    }
    pub(crate) fn record_pipeline_failure_step(
        &self,
        run: &JobRun,
        started_at: chrono::DateTime<Utc>,
        finished_at: chrono::DateTime<Utc>,
        message: &str,
    ) -> Result<(), OrbitError> {
        self.record_pipeline_diagnostic_step(
            run,
            started_at,
            finished_at,
            None,
            message,
            JobRunState::Failed,
        )
    }
    /// [ORB-10002] Record a terminal diagnostic step with an explicit state
    /// (`failed` for job errors, `interrupted` for orphan reconciliation).
    pub(crate) fn record_pipeline_diagnostic_step(
        &self,
        run: &JobRun,
        started_at: chrono::DateTime<Utc>,
        finished_at: chrono::DateTime<Utc>,
        error_code: Option<&str>,
        message: &str,
        state: JobRunState,
    ) -> Result<(), OrbitError> {
        record::diagnostic_step(
            self.stores().jobs(),
            run,
            started_at,
            finished_at,
            error_code,
            message,
            state,
        )
    }
    /// [ORB-12038] Read a run's own `<run_id>.worker.log`, for a caller (`orbit
    /// run logs`) that found no audited CLI-invocation blobs to show. A run
    /// that fails before any step runs — a routine-dispatch workspace
    /// mismatch, a worker that could not start at all — has no step-scoped
    /// output to audit; the worker log is where that process wrote its own
    /// stderr, and it is the only place the cause exists.
    ///
    /// `Ok(None)` means no such file exists (the ordinary case for a run that
    /// reached step execution). `Some` with `content: None` means the file
    /// exists but its content could not be recovered (unreadable or empty);
    /// the caller still has the path to name where to look by hand.
    pub fn read_pipeline_worker_log(
        &self,
        run_id: &str,
    ) -> Result<Option<PipelineWorkerLogSnapshot>, OrbitError> {
        let path = pipeline_worker_log_path(&self.paths().logs_dir, run_id)?;
        if !path.is_file() {
            return Ok(None);
        }
        let mut file = File::open(&path).map_err(|error| {
            OrbitError::Io(format!(
                "open pipeline worker log '{}': {error}",
                path.display()
            ))
        })?;
        let content = read_pipeline_worker_log_tail(&mut file);
        Ok(Some(PipelineWorkerLogSnapshot { path, content }))
    }
    /// Worker supervision for this runtime's workspace.
    ///
    /// Built per call: supervision is a short-lived unit of work, and a fresh
    /// one always reflects the runtime's current handles.
    fn pipeline_worker_supervisor(&self) -> PipelineWorkerSupervisor {
        PipelineWorkerSupervisor::new(
            Arc::clone(&self.stores().job_run),
            Arc::clone(&self.stores().audit_event),
            self.paths().clone(),
            self.event_log.clone(),
            WorkerCommandConfig::for_paths(self.paths()),
            Arc::new(self.clone()),
        )
    }
    pub(super) fn spawn_pipeline_worker(
        &self,
        run_id: &str,
        actor: Option<&str>,
    ) -> Result<(), OrbitError> {
        self.pipeline_worker_supervisor().spawn(run_id, actor)
    }
    /// Spawn an already-built worker command. Production spawns go through
    /// [`PipelineWorkerSupervisor::spawn`]; this is the runtime-shaped entry
    /// point in-crate tests use to launch a worker fixture.
    #[cfg(test)]
    pub(crate) fn spawn_pipeline_worker_process(
        &self,
        run_id: &str,
        actor: Option<&str>,
        command: Command,
        worker_log: PipelineWorkerLog,
    ) -> Result<u32, OrbitError> {
        self.pipeline_worker_supervisor()
            .spawn_process(run_id, actor, command, worker_log)
    }
    /// Runtime-shaped entry point for the cancellation-race test; the
    /// observer itself records this through the supervisor.
    #[cfg(all(test, unix))]
    pub(crate) fn record_pipeline_worker_cancellation_exit(
        &self,
        run: &JobRun,
        signal: i32,
        exit_status: &str,
        actor: Option<&str>,
    ) -> Result<bool, OrbitError> {
        self.pipeline_worker_supervisor()
            .record_cancellation_exit(run, signal, exit_status, actor)
    }
    pub(super) fn finalize_pipeline_worker_startup_failure(
        &self,
        run: &JobRun,
        message: &str,
        actor: Option<&str>,
    ) -> Result<(), OrbitError> {
        self.pipeline_worker_supervisor()
            .finalize_startup_failure(run, message, actor)
    }
    pub(crate) fn record_pipeline_audit(
        &self,
        tool_name: &str,
        target_id: Option<&str>,
        actor: Option<&str>,
        status: AuditEventStatus,
        arguments: Value,
        error_message: Option<String>,
    ) -> Result<(), OrbitError> {
        record::pipeline_audit(
            self.stores().audit_events(),
            &self.paths().repo_root,
            record::PipelineAuditRow {
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
