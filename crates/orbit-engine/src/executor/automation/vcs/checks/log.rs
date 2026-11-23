//! Failed-job log reads and provenance binding for landing recovery.

use std::path::Path;

use orbit_common::{OrbitError, PrivateVcsFailure, PrivateVcsFailureClass};
use serde_json::json;

use super::super::operations;
use super::super::operations::{CiCommitJobEvidence, CiJobLogAnswer, CiLogReadThrottle};
use super::log_cache::{self, JobLogKey};
use crate::context::RuntimeHost;

/// One failed job's log. A completed job's readable answer is reused from
/// the host's [`log_cache`] for its run attempt. A read that recovered no
/// text, which the log reader reports beside an empty log, is a failed read;
/// when the provider throttled it, the failure carries that typed throttle. `deadline_ms`, when
/// set, bounds every read subprocess by the completion's absolute deadline.
pub(super) fn job_log<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    run_id: u64,
    job_id: u64,
    job: &CiCommitJobEvidence,
    deadline_ms: Option<u64>,
) -> Result<String, OrbitError> {
    let mut input = json!({
        "run_id": run_id.to_string(),
        "job_id": job_id.to_string(),
        "workspace_path": workspace.to_string_lossy(),
    });
    if let Some(deadline_ms) = deadline_ms {
        input["deadline_ms"] = json!(deadline_ms);
    }
    let cache_key = JobLogKey::for_completed_job(job, run_id, job_id);
    let cached = cache_key
        .as_ref()
        .and_then(|key| log_cache::load(host.data_root(), key));
    let reused = cached.is_some();
    let response = match cached {
        Some(answer) => answer,
        None => CiJobLogAnswer::decode_response(
            host.run_private_vcs_operation(operations::CI_JOB_LOG, input)?,
        ),
    };
    if let Some(reason) = response.fallback_error.as_deref() {
        let message = format!("the log read recovered no text: {reason}");
        return Err(match response.read_throttle {
            CiLogReadThrottle::Known(throttle) => {
                OrbitError::PrivateVcs(Box::new(PrivateVcsFailure {
                    operation: operations::CI_JOB_LOG.to_string(),
                    class: PrivateVcsFailureClass::Other,
                    message,
                    read_throttle: Some(throttle),
                }))
            }
            // An unreadable throttle is no wait evidence: the read stays
            // the ordinary failure.
            CiLogReadThrottle::Unreadable(_) | CiLogReadThrottle::Absent => {
                OrbitError::Execution(message)
            }
        });
    }
    let unique_failed_step = job.failed_steps.value().and_then(|steps| {
        let [step] = steps.as_slice() else {
            return None;
        };
        step.text()
            .filter(|step| job.failed_step.text() == Some(step))
    });
    let log = response.bound_log(
        job.name.text(),
        super::jobs::failed_step(job),
        unique_failed_step,
    )?;
    // A completed job's readable answer never changes for its run attempt,
    // so later passes reuse it instead of reading the provider again.
    if let (false, Some(key)) = (reused, cache_key.as_ref()) {
        log_cache::store(host.data_root(), key, &response);
    }
    Ok(log)
}

/// Whether a currently failed workflow job has a readable, attributable log.
/// This only wakes the landing: classification and recovery still run there.
pub(in crate::executor::automation::vcs) fn failed_job_logs_readable<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    head: &str,
    failed: &[(Option<String>, String)],
    deadline_ms: u64,
) -> Result<bool, OrbitError> {
    let jobs = super::jobs::commit_jobs(host, workspace, head)?;
    for job in jobs.iter().filter(|job| {
        super::jobs::unsuccessful(job)
            && failed.iter().any(|check| super::jobs::answers(job, check))
    }) {
        if super::jobs::concluded_stopped(job).is_some() {
            return Ok(true);
        }
        let (Some(run_id), Some(job_id)) = (job.run_id.number(), job.job_id.number()) else {
            return Ok(false);
        };
        if job_log(host, workspace, run_id, job_id, job, Some(deadline_ms)).is_ok() {
            return Ok(true);
        }
    }
    Ok(false)
}
