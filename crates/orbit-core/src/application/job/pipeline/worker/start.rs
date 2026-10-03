//! Worker startup: workspace and store preflight, adoption at an upgrade, and
//! the spawn delegations to the supervisor.

use super::*;

pub(super) fn effective_strict_containment(
    configured: bool,
    override_for_run: bool,
    inherited: Option<&std::ffi::OsStr>,
) -> bool {
    configured || override_for_run || inherited == Some(std::ffi::OsStr::new("1"))
}

impl OrbitRuntime {
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
    pub(super) fn verify_routine_dispatch_workspace(&self, run: &JobRun) -> Result<(), OrbitError> {
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
    pub(super) fn record_routine_dispatch_workspace_mismatch(
        &self,
        run: &JobRun,
        error: &OrbitError,
    ) {
        let now = Utc::now();
        log_best_effort(
            "record workspace mismatch diagnostic",
            &run.run_id,
            self.record_pipeline_diagnostic_step(
                run,
                run.scheduled_at,
                now,
                Some(ROUTINE_DISPATCH_WORKSPACE_MISMATCH_ERROR_CODE),
                &error.to_string(),
                JobRunState::Cancelled,
            ),
        );
    }

    /// Reopen the shared SQLite store before the worker claims a run.
    ///
    /// Another Orbit process may advance the host-global database while this
    /// runtime remains alive. Reopening here applies migrations supported by
    /// this binary, and fails before any agent work when the database is
    /// newer than this binary — whether it refuses outright or would only
    /// open read-only, which a run that must write cannot use (ORB-12434).
    /// Invocation persistence still reopens independently, but it can no
    /// longer be the first compatibility check after useful work completes.
    pub(crate) fn preflight_pipeline_worker_store(&self) -> Result<(), OrbitError> {
        self.ensure_persistence_ready()?;
        Ok(())
    }

    /// Continue a run this process already owns, after its previous image
    /// handed it over at an upgrade. The run keeps its start, owner and
    /// checkpoints; only steps that had not completed run again.
    pub(super) fn adopt_pipeline_run(&self, run_id: &str) -> Result<(), OrbitError> {
        let run = self.show_job_run(run_id)?;
        if run.state != JobRunState::Running || run.pid != Some(std::process::id()) {
            return Err(OrbitError::Execution(format!(
                "pipeline worker cannot adopt run '{run_id}': it is {} and not owned by this \
                 process",
                run.state
            )));
        }
        let (yaml_path, _) = self.resolve_run_definition(&run)?;
        tracing::info!(
            target: "orbit.core.job_run",
            run_id,
            "adopted the run handed over by the replaced Orbit executable",
        );
        let started_at = run.started_at.unwrap_or(run.scheduled_at);
        self.execute_started_pipeline_run(&run, &yaml_path, started_at, false)
    }

    /// Worker supervision for this runtime's workspace.
    ///
    /// Built per call: supervision is a short-lived unit of work, and a fresh
    /// one always reflects the runtime's current handles.
    pub(super) fn pipeline_worker_supervisor(
        &self,
        strict_override: bool,
    ) -> PipelineWorkerSupervisor {
        // Tests substitute the worker program and must not reach the host's
        // service manager; the live containment test opts in on its own
        // supervisor.
        let settings = self.context.settings().worker_containment();
        let inherited = std::env::var_os(scope::STRICT_WORKER_CONTAINMENT_ENV);
        let strict =
            effective_strict_containment(settings.strict, strict_override, inherited.as_deref());
        let limits = if (cfg!(test) || worker_substituted_process_wide()) && !strict {
            None
        } else {
            scope::WorkerLimits::from_settings(settings)
        };
        PipelineWorkerSupervisor::new(
            Arc::clone(&self.stores().job_run),
            Arc::clone(&self.stores().audit_event),
            self.paths().clone(),
            self.event_log.clone(),
            WorkerCommandConfig::for_paths(self.paths())
                .contained(limits)
                .strict_containment(strict),
            Arc::new(self.clone()),
        )
    }

    /// Launch the worker for a claimed leaf run [ORB-12616].
    ///
    /// The caller must already hold this claim's trusted binding: the
    /// supervisor records it against the child's PID and sets
    /// `ORBIT_WORKER_CONTEXT_REQUIRED`, so the child refuses to run if it
    /// cannot resolve that binding back out of host authority.
    pub(crate) fn spawn_claimed_leaf_worker(&self, run_id: &str) -> Result<(), OrbitError> {
        if self.worker_invocation().is_none() {
            return Err(OrbitError::PolicyDenied(
                "a claimed leaf worker must be launched from a bound runtime".into(),
            ));
        }
        self.pipeline_worker_supervisor(false).spawn(run_id, None)
    }

    pub(in crate::application::job::pipeline) fn spawn_pipeline_worker(
        &self,
        run_id: &str,
        actor: Option<&str>,
        strict_override: bool,
    ) -> Result<(), OrbitError> {
        self.pipeline_worker_supervisor(strict_override)
            .spawn(run_id, actor)
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
        self.pipeline_worker_supervisor(false)
            .spawn_process(run_id, actor, command, worker_log)
    }

    pub(in crate::application::job::pipeline) fn finalize_pipeline_worker_startup_failure(
        &self,
        run: &JobRun,
        message: &str,
        error_code: Option<&str>,
        actor: Option<&str>,
    ) -> Result<(), OrbitError> {
        self.pipeline_worker_supervisor(false)
            .finalize_startup_failure(run, message, error_code, actor)
    }
}
