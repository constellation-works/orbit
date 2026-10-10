//! Diagnostic, resource-limit and cancellation records for a worker's run.

use super::*;

impl OrbitRuntime {
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
            scope::WorkerScopeCgroup::of_current_process().and_then(|scope| scope.limit_breach())
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
