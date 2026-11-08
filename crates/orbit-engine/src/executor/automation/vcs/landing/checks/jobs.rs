//! Workflow job matching, transient-log evidence and stop excerpts.

use std::path::Path;

use orbit_common::OrbitError;
use serde_json::json;

use crate::context::RuntimeHost;

use super::super::super::super::ci::log_signature::{
    LineKind, classify_log_lines, declared_infrastructure_signature, diagnostic_excerpt,
    log_payload,
};
use super::super::super::baseline::network_inconclusive;
use super::super::super::operations;
use super::super::super::operations::{CiCommitJobEvidence, CiCommitJobsAnswer, CiJobConclusion};

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

/// The step a failed job's signature falls back to naming.
pub(super) fn failed_step(job: &CiCommitJobEvidence) -> &str {
    job.failed_step
        .text()
        .or_else(|| job.name.text())
        .unwrap_or_default()
}

/// Every job on `commit`. An answer without a job list is a failed read,
/// never an empty one.
pub(super) fn commit_jobs<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    commit: &str,
) -> Result<Vec<CiCommitJobEvidence>, OrbitError> {
    let response = host.run_private_vcs_operation(
        operations::CI_COMMIT_JOBS,
        json!({"commit": commit, "workspace_path": workspace.to_string_lossy()}),
    )?;
    CiCommitJobsAnswer::decode_response(response)
        .map(|answer| answer.jobs)
        .ok_or_else(|| {
            OrbitError::Execution(format!("the jobs answer for {commit} carried no job list"))
        })
}

/// A job that finished without passing.
pub(super) fn unsuccessful(job: &CiCommitJobEvidence) -> bool {
    job.conclusion
        .value()
        .is_some_and(CiJobConclusion::unsuccessful)
}

/// Whether rollup entry `check` is this job's check run.
pub(super) fn answers(
    job: &CiCommitJobEvidence,
    (workflow, name): &(Option<String>, String),
) -> bool {
    job.name.text().map(str::trim) == Some(name.as_str())
        && workflow
            .as_deref()
            .is_none_or(|workflow| job.workflow.text().map(str::trim) == Some(workflow))
}

pub(super) fn same_job(left: &CiCommitJobEvidence, right: &CiCommitJobEvidence) -> bool {
    left.name.same_observation(&right.name) && left.workflow.same_observation(&right.workflow)
}

pub(super) fn job_label(job: &CiCommitJobEvidence) -> String {
    let name = job.name.text().unwrap_or("unnamed job");
    match job.workflow.text() {
        Some(workflow) => format!("{workflow} / {name}"),
        None => name.to_string(),
    }
}

/// A failed waiter whose log names only an awaited cancellation, timeout, or
/// the waiter's own deadline. Any other emitted line, including a dependency
/// record, is a different failure.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum AwaitedStop {
    /// `ci-await: … concluded cancelled`.
    Cancelled,
    /// `ci-await: … concluded timed_out`.
    TimedOut,
    /// `ci-await: timed out after …`.
    Timeout,
}

/// The awaited stop a log reports when every emitted line is that signal or
/// waiter bookkeeping. `None` when the log names anything else.
pub(super) fn exclusive_awaited_stop(log: &str) -> Option<AwaitedStop> {
    let mut found = None;
    for (kind, line) in classify_log_lines(log) {
        if matches!(
            kind,
            LineKind::RunCommand
                | LineKind::CommandBody
                | LineKind::ParamDump
                | LineKind::EndGroup
                | LineKind::GenericTrailer
                | LineKind::Bookkeeping
        ) {
            continue;
        }
        let payload = log_payload(line).trim();
        if payload.is_empty() || payload.starts_with("ci-await-api:") {
            continue;
        }
        if payload.starts_with("orbit-dependency-evidence:") {
            return None;
        }
        let body = payload.strip_prefix("ci-await:")?.trim();
        let signal = if body.starts_with("timed out") {
            AwaitedStop::Timeout
        } else {
            let token = body
                .rsplit_once("concluded ")
                .and_then(|(_, rest)| rest.split([':', ' ', '\t']).next())
                .unwrap_or("");
            match token {
                "cancelled" => AwaitedStop::Cancelled,
                "timed_out" => AwaitedStop::TimedOut,
                _ => return None,
            }
        };
        if found.is_some_and(|prior| prior != signal) {
            return None;
        }
        found = Some(signal);
    }
    found
}

/// A job the provider stopped, as opposed to one whose step failed.
pub(super) fn concluded_stopped(job: &CiCommitJobEvidence) -> Option<&str> {
    job.conclusion.value().and_then(CiJobConclusion::stopped)
}

/// Signature of a stop classified from the job conclusion, with no log.
pub(super) fn conclusion_signature(conclusion: &str) -> String {
    format!("concluded {conclusion}")
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

/// [`transient_evidence`], or the infrastructure signature this workspace
/// declares that a failed job's log carries. Declared signatures live in
/// configuration, so core knows no repository's scripts.
pub(super) fn infrastructure_evidence<H: RuntimeHost + ?Sized>(
    host: &H,
    log: &str,
) -> Option<String> {
    transient_evidence(log).or_else(|| {
        declared_infrastructure_signature(log, &host.pr_config().ci_infrastructure_signatures)
            .map(str::to_string)
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
                "'{}' ({}: {}, run {:?}, job {}) failed with signature `{}`; log excerpt:\n{}",
                job.label,
                job.class.as_str(),
                job.why,
                job.run_id,
                job.job_id,
                job.signature,
                job.excerpt
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let unreadable = jobs
        .iter()
        .filter(|job| job.class == CheckFailureClass::Unknown)
        .map(|job| format!("'{}'", job.label))
        .collect::<Vec<_>>();
    let unknown = if unreadable.is_empty() {
        String::new()
    } else {
        format!(
            "; unreadable job logs left unknown: {}",
            unreadable.join(", ")
        )
    };
    format!("classified `{}`: {details}{unknown}", class.as_str())
}
