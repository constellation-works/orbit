//! One unchanged-head retry when the owner cannot read a required job's log.

use std::{collections::BTreeSet, path::Path};

use orbit_common::OrbitError;
use orbit_types::{
    task::TaskHistoryEntry,
    workflow::{CI_UNREADABLE_LOG_CLASSIFICATION, CiRerunAdmission, CiRerunPlan, CiRerunRoot},
};

use crate::context::{HostEnvironment, ReviewGate, RunLifecycle};

use super::super::super::operations::CiCommitJobEvidence;
use super::super::{
    jobs::{answers, concluded_stopped, job_label, unsuccessful},
    log::job_log,
};
use super::{CiRerunStep, RerunPlanRequest, authenticated_plan, send::send_reserved_rerun};

pub(super) const UNREADABLE_LOG_RERUN_PENDING: &str =
    "CI retry was reserved for the unreadable failed-job log; observing without resending";

/// The immutable PR and workflow identity an unreadable-log retry
/// authenticates, as other reruns do.
pub(super) struct UnreadableLogRetry<'a> {
    pub(super) workspace: &'a Path,
    pub(super) tasks: &'a [String],
    pub(super) pr: u64,
    pub(super) head: &'a str,
    pub(super) head_branch: &'a str,
    pub(super) accepted_base: &'a str,
    pub(super) base_branch: &'a str,
    pub(super) deadline_ms: u64,
}

pub(super) fn unreadable_log_rerun_pending(
    history: &[TaskHistoryEntry],
    pr: &str,
    head: &str,
    jobs: &[CiCommitJobEvidence],
    failed_runs: &BTreeSet<u64>,
) -> bool {
    history
        .iter()
        .filter_map(CiRerunPlan::from_reservation_entry)
        .any(|plan| {
            plan.schema_version == 1
                && plan.pr_number == pr
                && plan.head_sha == head
                && failed_runs.contains(&plan.run_id)
                && plan
                    .roots
                    .iter()
                    .any(|root| root.classification == CI_UNREADABLE_LOG_CLASSIFICATION)
                && jobs.iter().any(|job| {
                    job.run_id.number() == Some(plan.run_id)
                        && job.run_attempt.number() == Some(plan.run_attempt)
                })
        })
}

pub(super) fn retry_unreadable_log<H: RunLifecycle + ReviewGate + HostEnvironment + ?Sized>(
    host: &H,
    retry: UnreadableLogRetry<'_>,
    jobs: &[CiCommitJobEvidence],
    failed: &[(Option<String>, String)],
    unreadable: &CiCommitJobEvidence,
) -> Result<Option<CiRerunStep>, OrbitError> {
    let UnreadableLogRetry {
        workspace,
        tasks,
        pr,
        head,
        head_branch,
        accepted_base,
        base_branch,
        deadline_ms,
    } = retry;
    let (Some(run_id), Some(run_attempt), Some(unreadable_id)) = (
        unreadable.run_id.number(),
        unreadable.run_attempt.number(),
        unreadable.job_id.number(),
    ) else {
        return Ok(None);
    };
    let mut roots = Vec::new();
    let mut required_root = false;
    for job in jobs.iter().filter(|job| {
        job.run_id.number() == Some(run_id)
            && job.run_attempt.number() == Some(run_attempt)
            && unsuccessful(job)
            && concluded_stopped(job).is_none()
    }) {
        let Some(job_id) = job.job_id.number() else {
            return Ok(None);
        };
        if job_id != unreadable_id
            && !matches!(
                job_log(host, workspace, run_id, job_id, job, Some(deadline_ms)),
                Err(OrbitError::Execution(_))
            )
        {
            continue;
        }
        required_root |= failed.iter().any(|check| answers(job, check));
        roots.push(CiRerunRoot {
            job_id,
            check: job_label(job),
            classification: CI_UNREADABLE_LOG_CLASSIFICATION.into(),
            signature: "job log unavailable".into(),
        });
    }
    if !required_root {
        return Ok(None);
    }
    let request = RerunPlanRequest {
        workspace,
        pr: pr.to_string(),
        head,
        head_branch,
        base_branch,
        accepted_base,
        run_id,
        roots: &roots,
        deadline_ms,
    };
    // The adapter authenticates the entire failed set. A readable sibling can
    // join only through the existing authenticated dependency proof.
    let Ok(plan) = authenticated_plan(host, &request)? else {
        return Ok(None);
    };
    if plan.run_attempt != run_attempt {
        return Ok(None);
    }
    let now_ms = super::super::super::completion_deadline::now_ms()?;
    match host.admit_ci_rerun(tasks, None, &plan, now_ms)? {
        CiRerunAdmission::Reserved
            if send_reserved_rerun(host, workspace, tasks, None, pr, &plan, deadline_ms)? =>
        {
            Ok(Some(CiRerunStep::Acted(Box::new(plan))))
        }
        CiRerunAdmission::Observe(prior)
            if prior.run_id == run_id
                && prior.run_attempt == run_attempt
                && prior
                    .roots
                    .iter()
                    .any(|root| root.classification == CI_UNREADABLE_LOG_CLASSIFICATION) =>
        {
            Ok(Some(CiRerunStep::Wait(UNREADABLE_LOG_RERUN_PENDING.into())))
        }
        // A spent budget keeps the unreadable-log stop from this poll.
        _ => Ok(None),
    }
}
