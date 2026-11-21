use super::super::{StreamedLog, StreamedLogCollector};
use super::{fallback::*, selection::*, stream::*};
use orbit_common::{OrbitError, PrivateVcsFailure, PrivateVcsFailureClass, ProviderReadThrottle};
use orbit_exec::ExecRequest;
use serde_json::Value;
/// One bounded log read, and where its bytes came from.
pub struct RunLogRead {
    pub log: StreamedLog,
    /// [`SOURCE_RUN_LOG`] or [`SOURCE_JOB_API_LOG`].
    pub source: &'static str,
    /// Identity of the job whose log was read. Empty for a run-scoped read.
    pub source_jobs: Vec<Value>,
    /// Why the fallback recovered nothing, already redacted. Set only when the
    /// read ends with no text: a fallback that found evidence reports none.
    pub fallback_error: Option<String>,
    /// The provider throttled a fallback read, so `fallback_error` is a read
    /// the caller's policy may repeat, not a refusal. Set only beside it.
    pub fallback_throttle: Option<ProviderReadThrottle>,
    /// The requested step could not be mapped in a raw job log (see
    /// `StreamedLog::selection_unsupported`).
    pub selection_unsupported: bool,
}

impl RunLogRead {
    /// The typed throttle refusal when the read ended with no text because a
    /// fallback read was throttled, whether by the host's quota guard or by
    /// GitHub: the same typed error a direct read of that call returns, so a
    /// public caller sees the resource and retry time instead of an
    /// empty-but-successful (or missing-selection) answer.
    pub fn throttle_refusal(&self) -> Option<OrbitError> {
        let throttle = self.fallback_throttle.clone()?;
        let message = self.fallback_error.clone()?;
        Some(OrbitError::PrivateVcs(Box::new(PrivateVcsFailure {
            operation: "github.run.logs".to_string(),
            class: PrivateVcsFailureClass::Other,
            message,
            read_throttle: Some(throttle),
        })))
    }
}

/// Read one bounded log excerpt, recovering from per-job logs when the
/// run-scoped read succeeds with no output or reports the known parent-run
/// readiness error for this exact run.
///
/// Other failures stay errors: auth, network, source-limit, and retired-run
/// failures must remain visible to the caller rather than being mistaken for
/// the one GitHub readiness gap this fallback can safely answer.
///
/// `cached_run_view` lets a caller that already holds a verified, freshly
/// fetched `gh run view` payload for this exact run (projected the same way
/// `fallback_jobs` would project its own) hand it in so the fallback never
/// re-queries GitHub for metadata the caller already has.
pub fn read_run_log(
    requests: &RunLogRequests,
    bounds: LogReadBounds,
    cached_run_view: Option<&Value>,
) -> Result<RunLogRead, OrbitError> {
    read_run_log_guarded(requests, bounds, cached_run_view, &|request| {
        Ok(request.clone())
    })
}

/// Read the same bounded log, with a caller guard before every subprocess,
/// including metadata and per-job fallback reads. The guard may clamp the
/// request timeout or refuse an exhausted absolute budget.
pub fn read_run_log_guarded(
    requests: &RunLogRequests,
    bounds: LogReadBounds,
    cached_run_view: Option<&Value>,
    guard: &dyn Fn(&ExecRequest) -> Result<ExecRequest, OrbitError>,
) -> Result<RunLogRead, OrbitError> {
    let log = match run_scoped_log(requests, bounds, guard) {
        Ok(log) => log,
        Err(error) if is_parent_run_log_readiness_error(&error, &requests.run_id) => {
            let empty = match &requests.selection {
                Some(selection) => StreamedLogCollector::scoped(
                    bounds.max_bytes,
                    bounds.max_evidence_lines,
                    selection.clone(),
                )
                .finish(),
                None => {
                    StreamedLogCollector::new(bounds.max_bytes, bounds.max_evidence_lines).finish()
                }
            };
            let read = recover_from_job_logs(
                requests,
                bounds,
                empty,
                Some(&error.to_string()),
                cached_run_view,
                guard,
            );
            return ensure_selection_found(requests, read);
        }
        Err(error) => return Err(error),
    };
    if requests.selection.is_some() {
        if log.selection_found {
            return Ok(RunLogRead {
                log,
                source: SOURCE_RUN_LOG,
                source_jobs: Vec::new(),
                fallback_error: None,
                fallback_throttle: None,
                selection_unsupported: false,
            });
        }
        if log.total_bytes > 0 {
            return Err(selection_not_found_error(
                requests,
                &log,
                None,
                log.selection_unsupported,
            ));
        }
        // An empty run-scoped read is the known blind spot: the scoped
        // selection may still be present in a verified job log.
        let read = recover_from_job_logs(requests, bounds, log, None, cached_run_view, guard);
        return ensure_selection_found(requests, read);
    }
    if !log.text.trim().is_empty() {
        return Ok(RunLogRead {
            log,
            source: SOURCE_RUN_LOG,
            source_jobs: Vec::new(),
            fallback_error: None,
            fallback_throttle: None,
            selection_unsupported: false,
        });
    }
    Ok(recover_from_job_logs(
        requests,
        bounds,
        log,
        None,
        cached_run_view,
        guard,
    ))
}

fn ensure_selection_found(
    requests: &RunLogRequests,
    read: RunLogRead,
) -> Result<RunLogRead, OrbitError> {
    if requests.selection.is_none() || read.log.selection_found {
        return Ok(read);
    }
    if let Some(refusal) = read.throttle_refusal() {
        return Err(refusal);
    }
    Err(selection_not_found_error(
        requests,
        &read.log,
        read.fallback_error.as_deref(),
        read.selection_unsupported || read.log.selection_unsupported,
    ))
}

fn selection_not_found_error(
    requests: &RunLogRequests,
    log: &StreamedLog,
    fallback_error: Option<&str>,
    selection_unsupported: bool,
) -> OrbitError {
    let Some(selection) = &requests.selection else {
        return OrbitError::InvalidInput("log selection not found".to_string());
    };
    let job = match &requests.job {
        Some(job) => format!(" job {job}"),
        None => String::new(),
    };
    let detail = if let Some(step) = &selection.step {
        if selection_unsupported {
            format!(
                "log step selection unsupported: \"{step}\" in run {}{job}: raw job logs name steps only by their `Run <command>` display group, not by workflow step name, so this step's identity and runtime boundaries cannot be reliably established in the raw log",
                requests.run_id
            )
        } else {
            format!(
                "log step not found: \"{step}\" in run {}{job}",
                requests.run_id
            )
        }
    } else if let Some((start, end)) = selection.byte_window {
        format!(
            "byte window {start}..{} matched no log bytes in run {}{job} ({} bytes read)",
            end.map(|end| end.to_string())
                .unwrap_or_else(|| "end".to_string()),
            requests.run_id,
            log.total_bytes
        )
    } else if let Some((start, end)) = selection.line_window {
        format!(
            "line window {}..{} matched no log lines in run {}{job} ({} bytes read)",
            start + 1,
            end.map(|end| (end + 1).to_string())
                .unwrap_or_else(|| "end".to_string()),
            requests.run_id,
            log.total_bytes
        )
    } else {
        format!(
            "log selection matched nothing in run {}{job}",
            requests.run_id
        )
    };
    let detail = match fallback_error {
        Some(error) => format!("{detail}; {error}"),
        None => detail,
    };
    OrbitError::InvalidInput(format!("{detail}; no log bytes returned"))
}

/// GitHub CLI's exact readiness response for a parent workflow that has not
/// completed. Including the requested numeric run in the match prevents a
/// message about some other run from authorizing the fallback.
fn is_parent_run_log_readiness_error(error: &OrbitError, run_id: &str) -> bool {
    let message = error.to_string();
    let Some(detail) = message.strip_prefix("execution failed: gh run view --log failed: ") else {
        return false;
    };
    let readiness =
        format!("run {run_id} is still in progress; logs will be available when it is complete");
    detail == readiness || detail == format!("failed to get run log: {readiness}")
}
