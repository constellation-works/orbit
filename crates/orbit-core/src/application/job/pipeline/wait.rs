use super::*;

const PIPELINE_WAIT_DEFAULT_TIMEOUT_SECONDS: u64 = 3600;
const PIPELINE_WAIT_MAX_TIMEOUT_SECONDS: u64 = 7200;
const PIPELINE_WAIT_DEFAULT_POLL_SECONDS: u64 = 5;
pub(super) const PIPELINE_WAIT_MIN_POLL_SECONDS: u64 = 1;

#[derive(Debug, Clone, Serialize)]
pub struct PipelineWaitResult {
    pub results: Vec<PipelineWaitEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PipelineWaitEntry {
    pub run_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pipeline: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Wait-envelope success token. Canonical spelling is `JobRunState::Success`
/// (`success`); `succeeded` is accepted so in-flight synthetic skip results
/// and older wait JSON keep matching [ORB-12255].
pub fn pipeline_wait_status_is_success(status: &str) -> bool {
    matches!(status, "success" | "succeeded")
}

pub fn pipeline_wait_status_is_settled(status: &str) -> bool {
    pipeline_wait_status_is_success(status)
        || matches!(status, "failed" | "cancelled" | "interrupted")
}

impl OrbitRuntime {
    pub fn wait_pipeline_runs(
        &self,
        run_ids: &[String],
        timeout_seconds: u64,
        poll_interval_seconds: u64,
        actor: Option<&str>,
    ) -> Result<PipelineWaitResult, OrbitError> {
        let started_payload = json!({
            "actor": actor,
            "run_ids": run_ids,
            "timeout_seconds": timeout_seconds,
        });
        self.record_pipeline_audit(
            "pipeline.wait.started",
            None,
            actor,
            AuditEventStatus::Success,
            started_payload,
            None,
        )?;

        let started_at = Instant::now();
        let timeout = Duration::from_secs(timeout_seconds);
        let poll = Duration::from_secs(poll_interval_seconds.max(PIPELINE_WAIT_MIN_POLL_SECONDS));

        loop {
            let snapshot = self.collect_pipeline_wait_entries(run_ids, false)?;
            if snapshot
                .iter()
                .all(|entry| pipeline_wait_status_is_settled(&entry.status))
            {
                let result = PipelineWaitResult { results: snapshot };
                self.record_pipeline_wait_finished(actor, &result)?;
                return Ok(result);
            }

            if started_at.elapsed() >= timeout {
                let result = PipelineWaitResult {
                    results: self.collect_pipeline_wait_entries(run_ids, true)?,
                };
                self.record_pipeline_wait_finished(actor, &result)?;
                return Ok(result);
            }

            thread::sleep(poll);
        }
    }
    pub fn normalize_pipeline_wait_timeout(raw: Option<u64>) -> Result<u64, OrbitError> {
        let timeout_seconds = raw.unwrap_or(PIPELINE_WAIT_DEFAULT_TIMEOUT_SECONDS);
        if timeout_seconds > PIPELINE_WAIT_MAX_TIMEOUT_SECONDS {
            return Err(OrbitError::InvalidInput(format!(
                "`timeout_seconds` must be <= {PIPELINE_WAIT_MAX_TIMEOUT_SECONDS}"
            )));
        }
        Ok(timeout_seconds)
    }
    pub fn normalize_pipeline_wait_poll_interval(raw: Option<u64>) -> u64 {
        raw.unwrap_or(PIPELINE_WAIT_DEFAULT_POLL_SECONDS)
            .max(PIPELINE_WAIT_MIN_POLL_SECONDS)
    }
    fn collect_pipeline_wait_entries(
        &self,
        run_ids: &[String],
        timeout_incomplete: bool,
    ) -> Result<Vec<PipelineWaitEntry>, OrbitError> {
        run_ids
            .iter()
            .map(|run_id| {
                let run = match self.show_job_run(run_id) {
                    Ok(run) => run,
                    Err(OrbitError::NotFound {
                        kind: NotFoundKind::JobRun,
                        ..
                    }) => {
                        return Ok(PipelineWaitEntry {
                            run_id: run_id.clone(),
                            status: "failed".to_string(),
                            finished_at: None,
                            duration_ms: None,
                            pipeline: None,
                            error: Some("unknown run".to_string()),
                        });
                    }
                    Err(error) => return Err(error),
                };

                let terminal = match run.state {
                    JobRunState::Success => Some(JobRunState::Success.to_string()),
                    JobRunState::Failed => Some(JobRunState::Failed.to_string()),
                    JobRunState::Cancelled => Some(JobRunState::Cancelled.to_string()),
                    JobRunState::Interrupted => Some(JobRunState::Interrupted.to_string()),
                    _ => None,
                };
                let status = match (terminal, timeout_incomplete) {
                    (Some(status), _) => status,
                    (None, true) => "timeout".to_string(),
                    (None, false) => run.state.to_string(),
                };
                let pipeline = if matches!(status.as_str(), "timeout") {
                    None
                } else {
                    self.read_run_state(run_id)?.map(|state| state.pipeline)
                };
                let error = if matches!(status.as_str(), "failed" | "cancelled" | "interrupted") {
                    let (code, message) = run
                        .steps
                        .iter()
                        .rev()
                        .find(|step| step.error_code.is_some() || step.error_message.is_some())
                        .map(|step| (step.error_code.clone(), step.error_message.clone()))
                        .unwrap_or((None, None));
                    match (code, message) {
                        (Some(code), Some(message)) => Some(format!("{code}: {message}")),
                        (Some(code), None) => Some(code),
                        (None, Some(message)) => Some(message),
                        (None, None) => None,
                    }
                } else {
                    None
                };
                Ok(PipelineWaitEntry {
                    run_id: run_id.clone(),
                    status,
                    finished_at: run.finished_at.map(|value| value.to_rfc3339()),
                    duration_ms: run.duration_ms,
                    pipeline,
                    error,
                })
            })
            .collect()
    }
    fn record_pipeline_wait_finished(
        &self,
        actor: Option<&str>,
        result: &PipelineWaitResult,
    ) -> Result<(), OrbitError> {
        let mut succeeded = 0usize;
        let mut failed = 0usize;
        let mut cancelled = 0usize;
        let mut timeout = 0usize;
        for entry in &result.results {
            match entry.status.as_str() {
                status if pipeline_wait_status_is_success(status) => succeeded += 1,
                "failed" => failed += 1,
                "cancelled" => cancelled += 1,
                "timeout" => timeout += 1,
                _ => {}
            }
        }

        self.record_pipeline_audit(
            "pipeline.wait.finished",
            None,
            actor,
            AuditEventStatus::Success,
            json!({
                "actor": actor,
                "results_summary": {
                    "succeeded": succeeded,
                    "failed": failed,
                    "cancelled": cancelled,
                    "timeout": timeout,
                },
            }),
            None,
        )
    }
}
