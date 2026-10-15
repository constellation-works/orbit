//! Failed-job log reads and provenance binding for landing recovery.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_tools::github_cli::{SOURCE_JOB_API_LOG, SOURCE_RUN_LOG};
use serde_json::{Value, json};

use super::super::operations;
use crate::context::RuntimeHost;

/// One failed job's log. A read that recovered no text, which the log
/// reader reports beside an empty log, is a failed read.
pub(super) fn job_log<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    run_id: u64,
    job_id: u64,
    job: &Value,
) -> Result<String, OrbitError> {
    let response = host.run_private_vcs_operation(
        operations::CI_JOB_LOG,
        json!({
            "run_id": run_id.to_string(),
            "job_id": job_id.to_string(),
            "workspace_path": workspace.to_string_lossy(),
        }),
    )?;
    if let Some(reason) = response["fallback_error"].as_str() {
        return Err(OrbitError::Execution(format!(
            "the log read recovered no text: {reason}"
        )));
    }
    if response["source_complete"] == false {
        return Err(OrbitError::Execution(
            "the job log source is incomplete".to_string(),
        ));
    }
    let selected = response["diagnostic"].as_str();
    if selected.is_none() && response["failure_regions"].is_object() {
        return bound_failure_regions(&response, job)
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                OrbitError::Execution(
                    "the job log has no complete attributable failure regions".to_string(),
                )
            });
    }
    let step = super::failed_step(job);
    if response["truncated"] == true && selected.is_none() {
        return Err(OrbitError::Execution(
            "the job log is truncated and has no complete failing command".to_string(),
        ));
    }
    let text = selected
        .or_else(|| response["log"].as_str())
        .ok_or_else(|| OrbitError::Execution("the log answer carried no log".to_string()))?;
    // gh's columns bind command evidence to the provider's failed step.
    // Raw job API logs have no columns; a unique complete failed command is
    // preferred there so successful setup cannot supply the cause.
    let mut selected_lines = Vec::new();
    for line in text.lines() {
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

/// Regions omit source bytes, so attribution must come from the collector's
/// complete-command proof and the provider's unique failed step, never from
/// the display tail. This is the same binding the CI sweep requires.
fn bound_failure_regions<'a>(response: &'a Value, job: &Value) -> Option<&'a str> {
    let regions = &response["failure_regions"];
    if response["source_complete"] != true
        || regions["kind"] != "runner_failure_regions"
        || regions["command_complete"] != true
        || regions["selection_complete"] != true
    {
        return None;
    }
    let text = regions["text"]
        .as_str()
        .filter(|text| !text.trim().is_empty())?;
    let steps = job["failed_steps"].as_array()?;
    let [step] = steps.as_slice() else {
        return None;
    };
    let step = step.as_str().filter(|step| !step.trim().is_empty())?;
    if job["failed_step"].as_str() != Some(step) {
        return None;
    }
    match response["source"].as_str()? {
        SOURCE_RUN_LOG => {
            let name = job["name"]
                .as_str()
                .filter(|name| !name.trim().is_empty())?;
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
        SOURCE_JOB_API_LOG => {}
        _ => return None,
    }
    Some(text)
}
