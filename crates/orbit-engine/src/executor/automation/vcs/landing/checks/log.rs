//! Failed-job log reads and provenance binding for landing recovery.

use std::path::Path;

use orbit_common::{OrbitError, PrivateVcsFailure, PrivateVcsFailureClass};
use serde_json::json;

use super::super::super::operations;
use super::super::super::operations::{
    CiCommitJobEvidence, CiJobLogAnswer, CiLogReadThrottle, CiLogSource,
};
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
    let response = CiJobLogAnswer::decode_response(match cached {
        Some(answer) => answer,
        None => host.run_private_vcs_operation(operations::CI_JOB_LOG, input)?,
    });
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
    if response.source_complete == Some(false) {
        return Err(OrbitError::Execution(
            "the job log source is incomplete".to_string(),
        ));
    }
    if matches!(response.source, Some(CiLogSource::Unknown(_))) {
        return Err(OrbitError::Execution(
            "the job log source is unknown".to_string(),
        ));
    }
    // A completed job's readable answer never changes for its run attempt,
    // so later passes reuse it instead of reading the provider again.
    if let (false, Some(key)) = (reused, cache_key.as_ref())
        && let Ok(answer) = serde_json::to_value(&response)
    {
        log_cache::store(host.data_root(), key, &answer);
    }
    let selected = response.diagnostic.as_deref();
    if selected.is_none() && response.failure_regions.is_some() {
        return bound_failure_regions(&response, job)
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                OrbitError::Execution(
                    "the job log has no complete attributable failure regions".to_string(),
                )
            });
    }
    let step = super::jobs::failed_step(job);
    if response.truncated == Some(true) && selected.is_none() {
        return Err(OrbitError::Execution(
            "the job log is truncated and has no complete failing command".to_string(),
        ));
    }
    let text = selected
        .or(response.log.as_deref())
        .ok_or_else(|| OrbitError::Execution("the log answer carried no log".to_string()))?;
    // gh's columns bind command evidence to the provider's failed step.
    // Raw job API logs have no columns; a unique complete failed command is
    // preferred there so successful setup cannot supply the cause.
    let mut selected_lines = Vec::new();
    for line in text.lines() {
        // A raw job API log can contain arbitrary tabs in printed commands or
        // output. Its ISO timestamp precedes the payload, so recognize that
        // shape before interpreting any tabs as gh's job/step columns.
        if timestamp_prefixed(line) || response.source == Some(CiLogSource::JobApiLog) {
            selected_lines.push(line);
            continue;
        }
        let mut columns = line.splitn(3, '\t');
        if let (Some(_), Some(line_step), Some(_)) =
            (columns.next(), columns.next(), columns.next())
            && line_step != step
            && line_step != "UNKNOWN STEP"
        {
            if selected.is_some() {
                return Err(OrbitError::Execution(
                    "the failing command does not match the failed step".to_string(),
                ));
            }
            continue;
        }
        selected_lines.push(line);
    }
    Ok(selected_lines.join("\n"))
}

fn timestamp_prefixed(line: &str) -> bool {
    line.split_once(' ').is_some_and(|(timestamp, _)| {
        timestamp.ends_with('Z') && timestamp.contains('T') && !timestamp.contains('\t')
    })
}

/// Regions omit source bytes, so attribution must come from the collector's
/// complete-command proof and the provider's unique failed step, never from
/// the display tail. This is the same binding the CI sweep requires.
fn bound_failure_regions<'a>(
    response: &'a CiJobLogAnswer,
    job: &CiCommitJobEvidence,
) -> Option<&'a str> {
    let regions = response.failure_regions.as_ref()?;
    if response.source_complete != Some(true) || !regions.selection_proven() {
        return None;
    }
    let text = regions
        .text
        .as_deref()
        .filter(|text| !text.trim().is_empty())?;
    let steps = job.failed_steps.value()?;
    let [step] = steps.as_slice() else {
        return None;
    };
    let step = step.text().filter(|step| !step.trim().is_empty())?;
    if job.failed_step.text() != Some(step) {
        return None;
    }
    match response.source.as_ref()? {
        CiLogSource::RunLog => {
            let name = job.name.text().filter(|name| !name.trim().is_empty())?;
            let mut label = None;
            for line in text.lines() {
                let mut columns = line.splitn(3, '\t');
                let (Some(line_job), Some(line_step), Some(_)) =
                    (columns.next(), columns.next(), columns.next())
                else {
                    return None;
                };
                if line_job != name
                    || (line_step != step && line_step != "UNKNOWN STEP")
                    || label.is_some_and(|label| label != line_step)
                {
                    return None;
                }
                label = Some(line_step);
            }
        }
        // Collection refuses raw evidence from multiple failed commands,
        // including when their other failures fall outside retained regions.
        CiLogSource::JobApiLog => {}
        CiLogSource::Unknown(_) => return None,
    }
    Some(text)
}
