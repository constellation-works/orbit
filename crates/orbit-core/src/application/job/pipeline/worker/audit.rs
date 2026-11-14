//! Diagnostic, resource-limit and cancellation records for a worker's run.

use super::*;
use crate::application::job::run::worker_scope::WorkerScopeCgroup;

impl OrbitRuntime {
    /// The bounded process snapshot captured when this run's watchdog tripped.
    /// Resource evidence is workspace-scoped and contains no argv or env.
    pub fn worker_resource_limit_evidence(
        &self,
        run_id: &str,
    ) -> Result<Option<orbit_store::contracts::WorkerResourceLimitEvidence>, OrbitError> {
        use orbit_store::contracts::WorkerResourceLimitStoreBackend;
        self.sqlite_store()?
            .worker_resource_limit_evidence(&self.workspace_id()?, run_id)
    }

    /// [ORB-12903] A run that failed after its worker scope hit a memory or
    /// task limit names that cause, under its own error code, ahead of the
    /// generic failure step finalization would write (the first recorded
    /// error wins). Only this process's own `orbit-worker-*` scope counts, so
    /// an uncontained worker never blames its launcher's other work on itself.
    pub(super) fn record_worker_resource_limit(
        &self,
        run: &JobRun,
        started_at: chrono::DateTime<Utc>,
        finished_at: chrono::DateTime<Utc>,
        outcome: Result<&crate::application::job::V2JobRunResult, &OrbitError>,
    ) {
        let failure = match outcome {
            Ok(result) if result.success => return,
            Ok(result) => result
                .message
                .clone()
                .unwrap_or_else(|| "job completed with success=false".to_string()),
            Err(error) => error.to_string(),
        };
        let Some(breach) =
            WorkerScopeCgroup::of_current_process().and_then(|scope| scope.limit_breach())
        else {
            return;
        };
        let message = format!("{}; run failure: {failure}", breach.describe());
        tracing::warn!(target: "orbit.core.job_run", run_id = run.run_id, "{message}");
        log_best_effort(
            "record resource limit diagnostic",
            &run.run_id,
            self.record_pipeline_diagnostic_step(
                run,
                started_at,
                finished_at,
                Some(scope::WORKER_RESOURCE_LIMIT_ERROR_CODE),
                &message,
                JobRunState::Failed,
            ),
        );
    }

    /// [ORB-12038] Read a run's own `<run_id>.worker.log`, for a caller (`orbit
    /// run logs`) that found no audited CLI-invocation blobs to show. A run
    /// that fails before any step runs — a routine-dispatch workspace
    /// mismatch, a worker that could not start at all — has no step-scoped
    /// output to audit; the worker log is where that process wrote its own
    /// stderr, and it is the only place the cause exists.
    ///
    /// `Ok(None)` means the path is not a regular file, including when it is
    /// absent. An error opening an existing file returns `Err(OrbitError::Io)`.
    /// After a successful open, `Ok(Some(...))` retains the path; `content: None`
    /// means the tail could not be read or yielded no content or diagnostics.
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
}
