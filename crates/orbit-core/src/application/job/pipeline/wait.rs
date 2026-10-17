use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, PoisonError};

use super::*;

const PIPELINE_WAIT_DEFAULT_TIMEOUT_SECONDS: u64 = 3600;
const PIPELINE_WAIT_MAX_TIMEOUT_SECONDS: u64 = 21600;
const PIPELINE_WAIT_DEFAULT_POLL_SECONDS: u64 = 5;
pub(super) const PIPELINE_WAIT_MIN_POLL_SECONDS: u64 = 1;

/// Set to `1` in a debug build to make a pipeline wait add each poll to its
/// elapsed time and return, instead of sleeping.
///
/// Release builds ignore it. The virtual clock does not wake when a worker is
/// reaped, so set it only on a deadline that must expire while the run is
/// still active. A wait that should observe completion keeps the real clock.
pub(crate) const PIPELINE_WAIT_VIRTUAL_ENV: &str = "ORBIT_TEST_PIPELINE_WAIT_VIRTUAL";

pub(crate) trait PipelineWaitClock {
    fn elapsed(&self) -> Duration;
    /// Sleep for up to `duration`, ending early once this process reaps a
    /// worker after the `exits_seen` count of `worker_exits_seen`.
    fn sleep(&self, duration: Duration, exits_seen: u64);
}

struct SystemPipelineWaitClock {
    started_at: Instant,
    /// Milliseconds the virtual clock has advanced. Absent unless
    /// [`PIPELINE_WAIT_VIRTUAL_ENV`] is `1` in a debug build.
    #[cfg(debug_assertions)]
    virtual_millis: Option<AtomicU64>,
}

impl PipelineWaitClock for SystemPipelineWaitClock {
    fn elapsed(&self) -> Duration {
        #[cfg(debug_assertions)]
        if let Some(virtual_millis) = &self.virtual_millis {
            return Duration::from_millis(virtual_millis.load(Ordering::Relaxed));
        }
        self.started_at.elapsed()
    }

    fn sleep(&self, duration: Duration, exits_seen: u64) {
        #[cfg(feature = "test-support")]
        if let Some(wait) = crate::runtime::pipeline_poll_wait_override() {
            wait(duration);
            return;
        }
        #[cfg(debug_assertions)]
        if let Some(virtual_millis) = &self.virtual_millis {
            let added = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
            virtual_millis.fetch_add(added, Ordering::Relaxed);
            return;
        }
        sleep_until_worker_exit(exits_seen, duration);
    }
}

fn system_pipeline_wait_clock() -> SystemPipelineWaitClock {
    SystemPipelineWaitClock {
        started_at: Instant::now(),
        #[cfg(debug_assertions)]
        virtual_millis: match std::env::var(PIPELINE_WAIT_VIRTUAL_ENV).ok().as_deref() {
            Some("1") => Some(AtomicU64::new(0)),
            _ => None,
        },
    }
}

/// Worker processes this process's startup observers have reaped [ORB-16033].
///
/// A worker exits only after it has written its run's outcome, and the
/// observer counts it only after reaping it and settling a run the exit left
/// non-terminal (a cancelled run is the canceller's to settle, so its waits
/// keep polling). The CLI or tool call that submitted a run owns that observer,
/// so its wait sleeps here instead of on a bare timer and rereads durable
/// state as soon as the worker is gone. A count change certifies nothing: the
/// wait only rereads, and any exit wakes every wait in the process. A wait
/// whose worker another process launched gets no wake and keeps its poll.
static WORKER_EXITS: (Mutex<u64>, Condvar) = (Mutex::new(0), Condvar::new());

/// The reaped-worker count. A wait reads it before each snapshot, so an exit
/// announced after that read, even before the sleep starts, cuts the sleep short.
pub(super) fn worker_exits_seen() -> u64 {
    *WORKER_EXITS
        .0
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Wake every wait in this process to reread durable run state.
pub(super) fn announce_worker_exit() {
    let (exits, changed) = &WORKER_EXITS;
    let mut count = exits.lock().unwrap_or_else(PoisonError::into_inner);
    *count = count.wrapping_add(1);
    drop(count);
    changed.notify_all();
}

/// Sleep for up to `duration` while the count still equals `exits_seen`.
/// Spurious condvar wakeups resume the sleep; only a count change or the
/// timeout ends it, so a missed wake costs at most the poll interval.
pub(super) fn sleep_until_worker_exit(exits_seen: u64, duration: Duration) {
    let (exits, changed) = &WORKER_EXITS;
    let count = exits.lock().unwrap_or_else(PoisonError::into_inner);
    drop(
        changed
            .wait_timeout_while(count, duration, |count| *count == exits_seen)
            .unwrap_or_else(PoisonError::into_inner),
    );
}

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

/// A child that ended `held` awaiting named evidence or a forge. It is settled
/// but neither a success nor a failure [ORB-14748].
pub fn pipeline_wait_status_is_held(status: &str) -> bool {
    status == "held"
}

pub fn pipeline_wait_status_is_settled(status: &str) -> bool {
    pipeline_wait_status_is_success(status)
        || pipeline_wait_status_is_held(status)
        || matches!(status, "failed" | "timeout" | "cancelled" | "interrupted")
}

impl OrbitRuntime {
    pub fn wait_pipeline_runs(
        &self,
        run_ids: &[String],
        timeout_seconds: u64,
        poll_interval_seconds: u64,
        actor: Option<&str>,
    ) -> Result<PipelineWaitResult, OrbitError> {
        self.wait_pipeline_runs_with_clock(
            run_ids,
            timeout_seconds,
            poll_interval_seconds,
            actor,
            &system_pipeline_wait_clock(),
        )
    }

    pub(crate) fn wait_pipeline_runs_with_clock(
        &self,
        run_ids: &[String],
        timeout_seconds: u64,
        poll_interval_seconds: u64,
        actor: Option<&str>,
        clock: &dyn PipelineWaitClock,
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

        let timeout = Duration::from_secs(timeout_seconds);
        let poll = Duration::from_secs(poll_interval_seconds.max(PIPELINE_WAIT_MIN_POLL_SECONDS));

        loop {
            let exits_seen = worker_exits_seen();
            // One stale-run pass per poll tick; collect then reads awaited
            // rows without repeating per-run owner probes.
            self.reconcile_stale_job_runs(None)?;
            let snapshot = self.collect_pipeline_wait_entries(run_ids, false)?;
            if snapshot
                .iter()
                .all(|entry| pipeline_wait_status_is_settled(&entry.status))
            {
                let result = PipelineWaitResult { results: snapshot };
                self.record_pipeline_wait_finished(actor, &result)?;
                return Ok(result);
            }

            let elapsed = clock.elapsed();
            if elapsed >= timeout {
                let result = PipelineWaitResult {
                    results: self.collect_pipeline_wait_entries(run_ids, true)?,
                };
                self.record_pipeline_wait_finished(actor, &result)?;
                return Ok(result);
            }

            clock.sleep(poll.min(timeout.saturating_sub(elapsed)), exits_seen);
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
                let run = match self.get_job_run_backend(run_id) {
                    Ok(Some(run)) => run,
                    Ok(None) => {
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

                // A caller deadline projects an active run as timeout without
                // changing its durable state. A durably timed-out run retains
                // its own terminal evidence even when the deadline has passed.
                let wait_incomplete = timeout_incomplete && !run.state.is_terminal();
                let status = if wait_incomplete {
                    "timeout".to_string()
                } else {
                    run.state.to_string()
                };
                let pipeline = if wait_incomplete {
                    None
                } else {
                    self.read_run_state(run_id)?.map(|state| state.pipeline)
                };
                let error = if matches!(
                    run.state,
                    JobRunState::Failed
                        | JobRunState::Timeout
                        | JobRunState::Cancelled
                        | JobRunState::Interrupted
                ) {
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
