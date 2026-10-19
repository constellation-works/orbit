//! Workflow job matching, transient-log evidence and stop excerpts.

use std::path::Path;

use orbit_common::OrbitError;
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::super::super::ci::log_signature::{
    LineKind, classify_log_lines, diagnostic_excerpt,
};
use super::super::super::baseline::network_inconclusive;
use super::super::super::operations;

use super::recovery::{CheckFailureClass, FailedJob};

/// Largest log excerpt a stop's evidence carries per failed job.
const MAX_EXCERPT_BYTES: usize = 2 * 1024;

/// Lines a runner prints when the machine, not the code, gave out.
const RUNNER_LOSS: &[&str] = &[
    "the runner has received a shutdown signal",
    "lost communication with the server",
    "the operation was canceled",
    "no space left on device",
    "the hosted runner encountered an error",
];

/// The one completed job in `jobs` that is `job`'s same job.
pub(super) fn single_completed_job<'a>(jobs: &'a [Value], job: &Value) -> Option<&'a Value> {
    let matching = jobs
        .iter()
        .filter(|candidate| same_job(candidate, job))
        .collect::<Vec<_>>();
    let [found] = matching.as_slice() else {
        return None;
    };
    (found["status"] == "completed").then_some(*found)
}

/// The step a failed job's signature falls back to naming.
pub(super) fn failed_step(job: &Value) -> &str {
    job["failed_step"]
        .as_str()
        .or_else(|| job["name"].as_str())
        .unwrap_or_default()
}

/// Every job on `commit`. An answer without a job list is a failed read,
/// never an empty one.
pub(super) fn commit_jobs<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    commit: &str,
) -> Result<Vec<Value>, OrbitError> {
    let response = host.run_private_vcs_operation(
        operations::CI_COMMIT_JOBS,
        json!({"commit": commit, "workspace_path": workspace.to_string_lossy()}),
    )?;
    response["jobs"].as_array().cloned().ok_or_else(|| {
        OrbitError::Execution(format!("the jobs answer for {commit} carried no job list"))
    })
}

/// A job that finished without passing.
pub(super) fn unsuccessful(job: &Value) -> bool {
    super::super::super::super::ci::unsuccessful_conclusion(job["conclusion"].as_str())
}

/// Whether rollup entry `check` is this job's check run.
pub(super) fn answers(job: &Value, (workflow, name): &(Option<String>, String)) -> bool {
    job["name"].as_str().map(str::trim) == Some(name.as_str())
        && workflow
            .as_deref()
            .is_none_or(|workflow| job["workflow"].as_str().map(str::trim) == Some(workflow))
}

pub(super) fn same_job(left: &Value, right: &Value) -> bool {
    left["name"] == right["name"] && left["workflow"] == right["workflow"]
}

pub(super) fn job_label(job: &Value) -> String {
    let name = job["name"].as_str().unwrap_or("unnamed job");
    match job["workflow"].as_str() {
        Some(workflow) => format!("{workflow} / {name}"),
        None => name.to_string(),
    }
}

/// The network or runner failure a log shows, if any.
pub(super) fn transient_evidence(log: &str) -> Option<String> {
    let emitted = classify_log_lines(log)
        .into_iter()
        .filter(|(kind, _)| {
            !matches!(
                kind,
                LineKind::RunCommand
                    | LineKind::CommandBody
                    | LineKind::ParamDump
                    | LineKind::EndGroup
            )
        })
        .map(|(_, line)| line)
        .collect::<Vec<_>>()
        .join("\n");
    network_inconclusive(&emitted).or_else(|| {
        let lowered = emitted.to_ascii_lowercase();
        RUNNER_LOSS
            .iter()
            .find(|marker| lowered.contains(*marker))
            .map(|marker| (*marker).to_string())
    })
}

/// Keep the selected emitted diagnostic even when cleanup dominates the tail.
pub(super) fn excerpt(log: &str) -> String {
    diagnostic_excerpt(log, MAX_EXCERPT_BYTES)
}

pub(super) fn describe(class: CheckFailureClass, jobs: &[FailedJob]) -> String {
    let details = jobs
        .iter()
        .map(|job| {
            format!(
                "'{}' ({}: {}) failed with signature `{}`; log excerpt:\n{}",
                job.label,
                job.class.as_str(),
                job.why,
                job.signature,
                job.excerpt
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("classified `{}`: {details}", class.as_str())
}
