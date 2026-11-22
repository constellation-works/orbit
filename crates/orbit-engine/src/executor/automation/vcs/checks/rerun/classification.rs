//! The unchanged-head classification rerun and the no-edit repair rerun.

use super::super::super::pr_answers::PullRequestStatus;
use super::super::{
    jobs::{answers, commit_jobs, failed_step, job_label, unsuccessful},
    log::job_log,
};
use super::conclusion::conclusion_root;
use super::send::send_reserved_rerun;
use super::{
    CiRerunStep, PlanMismatch, RerunPlanRequest, authenticated_plan, is_no_authenticated_plan_error,
};
use crate::context::RuntimeHost;
use orbit_common::OrbitError;
use orbit_types::workflow::{CiRerunAdmission, CiRerunPlan, CiRerunRefusal, CiRerunRoot};
use std::{collections::BTreeSet, path::Path};

/// [model ranking §5.1] Rerun a failed set once on its unchanged head when no
/// evidence has established its cause yet, so the second result can tell a
/// flaky failure from the head's own. Every failed job must be in one
/// workflow run; the provider adapter authenticates the set exactly as for a
/// flaky retry, and the shared one-rerun budget of the head is reserved
/// before the send. `Ok(None)` when the set is not eligible or the budget
/// is already spent; the caller then keeps its own route.
#[allow(
    clippy::too_many_arguments,
    reason = "the classification rerun takes the same landing identity the flaky retry does"
)]
pub(in crate::executor::automation::vcs::checks) fn request_classification_rerun<
    H: RuntimeHost + ?Sized,
>(
    host: &H,
    workspace: &Path,
    tasks: &[String],
    handoff: Option<&str>,
    status: &PullRequestStatus,
    base: &str,
    base_branch: &str,
    deadline_ms: u64,
    failed: &[super::super::recovery::FailedJob],
) -> Result<Option<Box<CiRerunPlan>>, OrbitError> {
    let (Some(head), Some(pr), Some(branch)) = (
        status.head_sha.as_deref(),
        status.number,
        status.head_branch.as_deref(),
    ) else {
        return Ok(None);
    };
    let runs = failed.iter().map(|job| job.run_id).collect::<BTreeSet<_>>();
    let [Some(run_id)] = runs.into_iter().collect::<Vec<_>>()[..] else {
        return Ok(None);
    };
    if failed.iter().any(|job| job.signature.is_empty()) {
        return Ok(None);
    }
    let roots = failed
        .iter()
        .map(|job| CiRerunRoot {
            job_id: job.job_id,
            check: job.label.clone(),
            classification: "unattributed".into(),
            signature: job.signature.clone(),
        })
        .collect::<Vec<_>>();
    let request = RerunPlanRequest {
        workspace,
        pr: pr.to_string(),
        head,
        head_branch: branch,
        base_branch,
        accepted_base: base,
        run_id,
        roots: &roots,
        deadline_ms,
    };
    let plan = match authenticated_plan(host, &request) {
        Ok(Ok(plan)) => plan,
        Ok(Err(PlanMismatch)) => return Ok(None),
        Err(error) => return Err(error),
    };
    match host.admit_ci_rerun(
        tasks,
        handoff,
        &plan,
        super::super::super::completion_deadline::now_ms()?,
    )? {
        CiRerunAdmission::Reserved
            if send_reserved_rerun(host, workspace, tasks, handoff, pr, &plan, deadline_ms)? =>
        {
            Ok(Some(Box::new(plan)))
        }
        _ => Ok(None),
    }
}

/// Rerun the failed required checks once on the unchanged head after an
/// admitted auto-fix repair made no edit. The repair agent's no-edit outcome
/// is the classification: the failure is not the candidate's to repair in
/// source. The provider adapter still authenticates the complete failed
/// set, and the shared one-rerun budget of the head is reserved before the
/// send, so a repeat failure cannot rerun again through this route.
#[allow(
    clippy::too_many_arguments,
    reason = "the no-edit rerun takes the same landing identity the classification rerun does"
)]
pub(in crate::executor::automation::vcs) fn request_no_edit_repair_rerun<
    H: RuntimeHost + ?Sized,
>(
    host: &H,
    workspace: &Path,
    tasks: &[String],
    status: &PullRequestStatus,
    failed: &[(Option<String>, String)],
    base: &str,
    base_branch: &str,
    deadline_ms: u64,
) -> Result<CiRerunStep, OrbitError> {
    let (Some(head), Some(pr), Some(branch)) = (
        status.head_sha.as_deref(),
        status.number,
        status.head_branch.as_deref(),
    ) else {
        return Ok(CiRerunStep::Wait(
            "CI retry requires exact PR/head/branch identity".into(),
        ));
    };
    let jobs = commit_jobs(host, workspace, head)?;
    if failed.iter().any(|check| {
        !jobs
            .iter()
            .any(|job| unsuccessful(job) && answers(job, check))
    }) {
        return Ok(CiRerunStep::NotEligible);
    }
    let failed_required_jobs = jobs
        .iter()
        .filter(|job| unsuccessful(job) && failed.iter().any(|check| answers(job, check)))
        .collect::<Vec<_>>();
    let runs = failed_required_jobs
        .iter()
        .filter_map(|job| job.run_id.number())
        .collect::<BTreeSet<_>>();
    let [run_id] = runs.into_iter().collect::<Vec<_>>()[..] else {
        return Ok(CiRerunStep::Wait(
            "CI retry holds: the failed checks are not in one workflow run".into(),
        ));
    };
    let run_failed_jobs = jobs
        .iter()
        .filter(|job| unsuccessful(job) && job.run_id.number() == Some(run_id))
        .collect::<Vec<_>>();
    let mut roots = Vec::new();
    for job in &run_failed_jobs {
        let Some(id) = job.job_id.number() else {
            return Ok(CiRerunStep::Wait(
                "CI retry job identity is incomplete".into(),
            ));
        };
        if super::super::jobs::concluded_stopped(job).is_some() {
            let Some(mut root) = conclusion_root(job) else {
                return Ok(CiRerunStep::Wait(
                    "CI retry job identity is incomplete".into(),
                ));
            };
            root.classification = "no_edit_repair".into();
            roots.push(root);
            continue;
        }
        let step = failed_step(job);
        let signature = if step.is_empty() {
            job_label(job)
        } else {
            format!("failed step {step}")
        };
        roots.push(CiRerunRoot {
            job_id: id,
            check: job_label(job),
            classification: "no_edit_repair".into(),
            signature,
        });
    }
    if roots.is_empty() {
        return Ok(CiRerunStep::NotEligible);
    }
    let request = RerunPlanRequest {
        workspace,
        pr: pr.to_string(),
        head,
        head_branch: branch,
        base_branch,
        accepted_base: base,
        run_id,
        roots: &roots,
        deadline_ms,
    };
    let plan = match authenticated_plan(host, &request) {
        Ok(Ok(plan)) => plan,
        Ok(Err(PlanMismatch)) => {
            return Ok(CiRerunStep::Wait(
                "CI retry plan differs from the exact classified request".into(),
            ));
        }
        Err(error) => {
            if is_no_authenticated_plan_error(&error) {
                return Err(error);
            }
            if error
                .private_vcs_failure()
                .is_some_and(|failure| failure.read_throttle.is_some())
            {
                return Err(error);
            }
            return Ok(CiRerunStep::Unreadable(format!(
                "CI retry holds: the complete failed-job set was not authenticated ({error})"
            )));
        }
    };
    match host.admit_ci_rerun(
        tasks,
        None,
        &plan,
        super::super::super::completion_deadline::now_ms()?,
    )? {
        CiRerunAdmission::Reserved => {
            if send_reserved_rerun(host, workspace, tasks, None, pr, &plan, deadline_ms)? {
                Ok(CiRerunStep::Acted(Box::new(plan)))
            } else {
                Ok(CiRerunStep::Wait(
                    "CI retry reservation or current owner authority changed before send".into(),
                ))
            }
        }
        CiRerunAdmission::Observe(previous) => {
            // The authenticated plan is built from the current completed
            // failed jobs. Only a later attempt than the reserved rerun's
            // original attempt (or a distinct failed run on the same head) is
            // that rerun's result; the original attempt keeps observing without
            // a second send.
            if (previous.run_id == plan.run_id && plan.run_attempt > previous.run_attempt)
                || previous.run_id != plan.run_id
            {
                Ok(CiRerunStep::NoEditRerunFailed(no_edit_rerun_failed_reason(
                    host,
                    workspace,
                    &plan.head_sha,
                    run_id,
                    failed,
                    &run_failed_jobs,
                    deadline_ms,
                )))
            } else {
                Ok(CiRerunStep::Wait(format!(
                    "CI retry was reserved for run {} attempt {} on head {}; observing without resending, including after an unknown reply",
                    previous.run_id, previous.run_attempt, previous.head_sha
                )))
            }
        }
        CiRerunAdmission::Backoff { not_before_ms } => Ok(CiRerunStep::Backoff(format!(
            "infrastructure CI retry backs off until {not_before_ms} ms"
        ))),
        CiRerunAdmission::InfrastructureExhausted { attempts } => {
            Ok(CiRerunStep::BudgetSpent(format!(
                "ci_infrastructure_retry_exhausted: classification=transient attempts={attempts}; the candidate stays in review"
            )))
        }
        CiRerunAdmission::Refused(CiRerunRefusal::SharedBudgetSpent) => {
            Ok(CiRerunStep::NoEditRerunFailed(no_edit_rerun_failed_reason(
                host,
                workspace,
                &plan.head_sha,
                run_id,
                failed,
                &run_failed_jobs,
                deadline_ms,
            )))
        }
        CiRerunAdmission::Refused(
            reason @ (CiRerunRefusal::InvalidAuthorization
            | CiRerunRefusal::UnreadableReservation
            | CiRerunRefusal::UnsupportedVersion
            | CiRerunRefusal::ConflictingReservations
            | CiRerunRefusal::UnreadableLegacyHistory
            | CiRerunRefusal::MissingLegacyActionIdentity
            | CiRerunRefusal::RequiresOwningHost
            | CiRerunRefusal::OwningTaskTerminal
            | CiRerunRefusal::LedgerAbsent),
        ) => Ok(CiRerunStep::Wait(format!("CI retry holds: {reason}"))),
    }
}

fn no_edit_rerun_failed_reason<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    head_sha: &str,
    run_id: u64,
    failed: &[(Option<String>, String)],
    run_failed_jobs: &[&super::super::super::operations::CiCommitJobEvidence],
    deadline_ms: u64,
) -> String {
    const MAX_REPORTED_TESTS: usize = 20;
    let mut check_names = BTreeSet::new();
    for job in run_failed_jobs {
        check_names.insert(job_label(job));
    }
    for (workflow, name) in failed {
        if !run_failed_jobs
            .iter()
            .any(|job| answers(job, &(workflow.clone(), name.clone())))
        {
            match workflow {
                Some(workflow) => {
                    check_names.insert(format!("{workflow} / {name}"));
                }
                None => {
                    check_names.insert(name.clone());
                }
            }
        }
    }
    let mut test_names = Vec::new();
    for job in run_failed_jobs {
        let Some(id) = job.job_id.number() else {
            continue;
        };
        if super::super::jobs::concluded_stopped(job).is_some() {
            continue;
        }
        if let Ok(log) = job_log(host, workspace, run_id, id, job, Some(deadline_ms)) {
            for name in super::super::super::super::ci::log_signature::failing_test_names(&log) {
                if !test_names.iter().any(|existing| existing == &name) {
                    test_names.push(name);
                }
            }
        }
    }
    let checks_text = check_names.into_iter().collect::<Vec<_>>().join(", ");
    let tests_suffix = if test_names.is_empty() {
        String::new()
    } else if test_names.len() > MAX_REPORTED_TESTS {
        let omitted = test_names.len() - MAX_REPORTED_TESTS;
        format!(
            " (failing tests: {}, +{omitted} more)",
            test_names[..MAX_REPORTED_TESTS].join(", ")
        )
    } else {
        format!(" (failing tests: {})", test_names.join(", "))
    };
    format!(
        "the CI repair made no edit and wrote no scope record; its one rerun of the \
         failed checks on head {head_sha} failed again for [{checks_text}]{tests_suffix}, \
         so no further repair or rerun runs for this head"
    )
}
