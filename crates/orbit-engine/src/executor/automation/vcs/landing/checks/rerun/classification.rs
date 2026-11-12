//! The unchanged-head classification rerun and the no-edit repair rerun.

use super::super::super::super::operations;
use super::super::super::super::pr::PullRequestStatus;
use super::super::{
    jobs::{answers, commit_jobs, failed_step, job_label, unsuccessful},
    log::job_log,
};
use super::conclusion::conclusion_root;
use super::send::send_reserved_rerun;
use super::{CiRerunStep, SHARED_RETRY_BUDGET_SPENT};
use crate::context::RuntimeHost;
use orbit_common::OrbitError;
use orbit_types::workflow::{CiRerunAdmission, CiRerunPlan, CiRerunRoot};
use serde_json::json;
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
pub(in crate::executor::automation::vcs::landing::checks) fn request_classification_rerun<
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
    let request = json!({"workspace_path":workspace,"pr":pr.to_string(),"head_sha":head,"head_branch":branch,"base_branch":base_branch,"accepted_base":base,"run_id":run_id,"roots":roots,"deadline_ms":deadline_ms});
    let response = host.run_private_vcs_operation(operations::CI_RERUN_PLAN, request)?;
    let plan: CiRerunPlan = serde_json::from_value(response["plan"].clone()).map_err(|_| {
        OrbitError::Execution("CI retry adapter returned no authenticated plan".into())
    })?;
    if plan.schema_version != 1
        || plan.head_sha != head
        || plan.pr_number != pr.to_string()
        || plan.head_branch != branch
        || plan.base_branch != base_branch
        || plan.accepted_base != base
        || plan.run_id != run_id
        || plan.roots != roots
    {
        return Ok(None);
    }
    match host.admit_ci_rerun(
        tasks,
        handoff,
        &plan,
        super::super::super::super::completion_deadline::now_ms()?,
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
    let failed_jobs = jobs
        .iter()
        .filter(|job| unsuccessful(job) && failed.iter().any(|check| answers(job, check)))
        .collect::<Vec<_>>();
    let runs = failed_jobs
        .iter()
        .filter_map(|job| job.run_id.number())
        .collect::<BTreeSet<_>>();
    let [run_id] = runs.into_iter().collect::<Vec<_>>()[..] else {
        return Ok(CiRerunStep::Wait(
            "CI retry holds: the failed checks are not in one workflow run".into(),
        ));
    };
    let mut roots = Vec::new();
    for job in &failed_jobs {
        let Some(id) = job.job_id.number() else {
            return Ok(CiRerunStep::Wait(
                "CI retry job identity is incomplete".into(),
            ));
        };
        if super::super::jobs::concluded_stopped(job).is_some() {
            let Some(root) = conclusion_root(job) else {
                return Ok(CiRerunStep::Wait(
                    "CI retry job identity is incomplete".into(),
                ));
            };
            roots.push(root);
            continue;
        }
        let log = job_log(host, workspace, run_id, id, job, Some(deadline_ms))?;
        let signature = super::super::super::super::super::ci::log_signature::error_signature(
            &log,
            failed_step(job),
        );
        if signature.step_fallback || signature.text.is_empty() {
            return Ok(CiRerunStep::NotEligible);
        }
        roots.push(CiRerunRoot {
            job_id: id,
            check: job_label(job),
            classification: "unattributed".into(),
            signature: signature.text,
        });
    }
    if roots.is_empty() {
        return Ok(CiRerunStep::NotEligible);
    }
    let request = json!({"workspace_path":workspace,"pr":pr.to_string(),"head_sha":head,"head_branch":branch,"base_branch":base_branch,"accepted_base":base,"run_id":run_id,"roots":roots,"deadline_ms":deadline_ms});
    let response = match host.run_private_vcs_operation(operations::CI_RERUN_PLAN, request) {
        Ok(response) => response,
        Err(error) => {
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
    let plan: CiRerunPlan = serde_json::from_value(response["plan"].clone()).map_err(|_| {
        OrbitError::Execution("CI retry adapter returned no authenticated plan".into())
    })?;
    if plan.schema_version != 1
        || plan.head_sha != head
        || plan.pr_number != pr.to_string()
        || plan.head_branch != branch
        || plan.base_branch != base_branch
        || plan.accepted_base != base
        || plan.run_id != run_id
        || plan.roots != roots
    {
        return Ok(CiRerunStep::Wait(
            "CI retry plan differs from the exact classified request".into(),
        ));
    }
    match host.admit_ci_rerun(
        tasks,
        None,
        &plan,
        super::super::super::super::completion_deadline::now_ms()?,
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
            // original attempt is that rerun's result; the original attempt
            // (or an ambiguous send with no later attempt yet) keeps
            // observing without a second send.
            if previous.run_id == plan.run_id && plan.run_attempt > previous.run_attempt {
                Ok(CiRerunStep::BudgetSpent(format!(
                    "the CI repair made no edit and wrote no scope record; its one rerun of the \
                     failed checks on head {} failed again, so no further repair or rerun runs \
                     for this head",
                    plan.head_sha
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
        CiRerunAdmission::Refused(reason) => {
            if reason == SHARED_RETRY_BUDGET_SPENT {
                Ok(CiRerunStep::BudgetSpent(format!(
                    "the no-edit repair rerun is spent: {reason}"
                )))
            } else {
                Ok(CiRerunStep::Wait(format!("CI retry holds: {reason}")))
            }
        }
    }
}
