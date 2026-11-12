//! Failure sets whose conclusions or failed-step names are the
//! classification, read without the failed jobs' logs.

use super::super::super::super::operations::{CiCommitJobEvidence, CiJobConclusion};
use super::super::rerun_diagnostic::{CiRerunDiagnostic, CiRerunStage};
use super::super::{
    jobs::{answers, job_label, unsuccessful},
    log::job_log,
    recovery::CheckFailureClass,
};
use crate::context::RuntimeHost;
use orbit_common::OrbitError;
use orbit_types::workflow::CiRerunRoot;
use std::{collections::BTreeSet, path::Path};

/// Jobs in `runs` whose conclusions alone are a transient stop, including a
/// failure job that only reports awaiting that stop. `None` keeps today's
/// log classification. A failure-job log that cannot be read is still an error.
pub(super) fn try_conclusion_transient<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    jobs: &[CiCommitJobEvidence],
    runs: &BTreeSet<u64>,
    deadline_ms: u64,
    diagnostic: &mut CiRerunDiagnostic,
) -> Result<Option<(u64, Vec<CiRerunRoot>)>, OrbitError> {
    if runs.len() != 1 {
        return Ok(None);
    }
    let Some(run_id) = runs.iter().next().copied() else {
        return Ok(None);
    };
    let mut stopped = Vec::new();
    let mut failures = Vec::new();
    for job in jobs
        .iter()
        .filter(|job| job.run_id.number() == Some(run_id))
    {
        if !unsuccessful(job) {
            continue;
        }
        if super::super::jobs::concluded_stopped(job).is_some() {
            stopped.push(job);
        } else if job.conclusion.value() == Some(&CiJobConclusion::Failure) {
            failures.push(job);
        } else {
            return Ok(None);
        }
    }
    if stopped.is_empty() && failures.is_empty() {
        return Ok(None);
    }
    let mut roots = stopped
        .iter()
        .filter_map(|job| conclusion_root(job))
        .collect::<Vec<_>>();
    if roots.len() != stopped.len() {
        return Ok(None);
    }
    if failures.is_empty() {
        return Ok(Some((run_id, roots)));
    }
    let mut saw_timeout = false;
    let mut needs_cancelled = false;
    let mut needs_timed_out = false;
    for job in &failures {
        let (Some(run), Some(id)) = (job.run_id.number(), job.job_id.number()) else {
            return Ok(None);
        };
        diagnostic.stage = CiRerunStage::Log;
        diagnostic.run_id = Some(run);
        diagnostic.job_id = Some(id);
        let log = job_log(host, workspace, run, id, job, Some(deadline_ms))?;
        diagnostic.stage = CiRerunStage::Classification;
        let Some(signal) = super::super::jobs::exclusive_awaited_stop(&log) else {
            return Ok(None);
        };
        let signature = super::super::super::super::super::ci::log_signature::error_signature(
            &log,
            super::super::jobs::failed_step(job),
        );
        if signature.step_fallback {
            return Ok(None);
        }
        match signal {
            super::super::jobs::AwaitedStop::Timeout => saw_timeout = true,
            super::super::jobs::AwaitedStop::Cancelled => needs_cancelled = true,
            super::super::jobs::AwaitedStop::TimedOut => needs_timed_out = true,
        }
        roots.push(CiRerunRoot {
            job_id: id,
            check: job_label(job),
            classification: CheckFailureClass::Transient.as_str().into(),
            signature: signature.text,
        });
    }
    let have_cancelled = stopped
        .iter()
        .any(|job| job.conclusion.value() == Some(&CiJobConclusion::Cancelled));
    let have_timed_out = stopped
        .iter()
        .any(|job| job.conclusion.value() == Some(&CiJobConclusion::TimedOut));
    if (needs_cancelled && !have_cancelled) || (needs_timed_out && !have_timed_out) {
        return Ok(None);
    }
    if stopped.is_empty() && !saw_timeout {
        return Ok(None);
    }
    Ok(Some((run_id, roots)))
}

pub(super) fn conclusion_root(job: &CiCommitJobEvidence) -> Option<CiRerunRoot> {
    let conclusion = super::super::jobs::concluded_stopped(job)?;
    Some(CiRerunRoot {
        job_id: job.job_id.number()?,
        check: job_label(job),
        classification: CheckFailureClass::Transient.as_str().into(),
        signature: super::super::jobs::conclusion_signature(conclusion),
    })
}

/// [ORB-23813] The rerun roots of a failed set in which every failed job
/// stopped in a dependency-handoff step: a step `patterns`
/// (`workflow.ci_infrastructure_retry.handoff_steps`) name, where the job
/// waits for or downloads another job's output. The provider's failed-step
/// name is the evidence, so no log is read and an unreadable one cannot turn
/// the set inconclusive.
///
/// `None` keeps today's per-job classification: no patterns, a failed check
/// no failed job answers, failed jobs in more than one run, or any
/// unsuccessful job in the run that is not a `failure` whose one failed step
/// matches (another step, several failed steps, or a cancelled or timed-out
/// conclusion, which keeps its own transient route).
pub(in crate::executor::automation::vcs) fn handoff_step_set(
    patterns: &[String],
    jobs: &[CiCommitJobEvidence],
    failed: &[(Option<String>, String)],
) -> Option<(u64, Vec<CiRerunRoot>)> {
    if patterns.is_empty() || failed.is_empty() {
        return None;
    }
    let mut runs = BTreeSet::new();
    for check in failed {
        let mut answering = jobs
            .iter()
            .filter(|job| unsuccessful(job) && answers(job, check))
            .peekable();
        answering.peek()?;
        for job in answering {
            runs.insert(job.run_id.number()?);
        }
    }
    let [run_id] = runs.into_iter().collect::<Vec<_>>()[..] else {
        return None;
    };
    let mut roots = Vec::new();
    for job in jobs
        .iter()
        .filter(|job| job.run_id.number() == Some(run_id) && unsuccessful(job))
    {
        if job.conclusion.value() != Some(&CiJobConclusion::Failure) {
            return None;
        }
        let step = handoff_step(patterns, job)?;
        roots.push(CiRerunRoot {
            job_id: job.job_id.number()?,
            check: job_label(job),
            classification: CheckFailureClass::Transient.as_str().into(),
            signature: orbit_types::workflow::ci_handoff_step_signature(step),
        });
    }
    (!roots.is_empty()).then_some((run_id, roots))
}

/// The provider's one failed step of `job` when `patterns` name it.
fn handoff_step<'a>(patterns: &[String], job: &'a CiCommitJobEvidence) -> Option<&'a str> {
    let step = job.failed_step.text()?;
    if let Some(steps) = job.failed_steps.value() {
        let [only] = steps.as_slice() else {
            return None;
        };
        if only.text() != Some(step) {
            return None;
        }
    }
    orbit_types::workflow::ci_handoff_step_matches(patterns, step).then_some(step)
}

pub(super) fn cancellation_timeout_reason(roots: &[CiRerunRoot]) -> String {
    let mut names = roots
        .iter()
        .map(|root| root.check.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    names.dedup();
    format!(
        "required CI jobs keep cancelling or timing out: {}",
        names.join(", ")
    )
}
