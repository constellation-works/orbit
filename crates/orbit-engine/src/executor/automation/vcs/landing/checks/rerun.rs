//! Shared first-failure planner; never reduce mixed jobs to a retry permission.

use super::super::super::operations::{CiCommitJobEvidence, CiJobConclusion};
use super::super::super::{operations, pr::PullRequestStatus};
use super::rerun_diagnostic::{self, CiRerunDiagnostic, CiRerunStage};
use super::{
    head::classify_logged_job,
    jobs::{answers, commit_jobs, failed_step, job_label, unsuccessful},
    log::job_log,
    recovery::CheckFailureClass,
};
use crate::context::RuntimeHost;
use orbit_common::OrbitError;
use orbit_types::workflow::{CiRerunAdmission, CiRerunPlan, CiRerunRoot};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub(in crate::executor::automation::vcs) enum CiRerunStep {
    NotEligible,
    /// A durable infrastructure backoff; it never expires into source repair.
    Backoff(String),
    Wait(String),
    /// [ORB-21586] The retry's evidence could not be read or authenticated,
    /// for this reason. Unlike [`CiRerunStep::Wait`], nothing is in flight:
    /// the landing bounds how often it repeats before it escalates.
    Unreadable(String),
    Acted(Box<CiRerunPlan>),
    /// Admitted reruns are spent. The reason names the stopped jobs or the
    /// exhausted infrastructure classification and count; no repair runs.
    BudgetSpent(String),
}

#[allow(
    clippy::too_many_arguments,
    reason = "the rerun planner takes pull status, failed jobs, base tip and the caller diagnostic together"
)]
pub(in crate::executor::automation::vcs) fn plan_ci_rerun<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    tasks: &[String],
    handoff: Option<&str>,
    status: &PullRequestStatus,
    failed: &[(Option<String>, String)],
    base: &str,
    tip: &str,
    base_branch: &str,
    deadline_ms: u64,
    classified: Option<&[super::recovery::FailedJob]>,
    execution: &str,
    observation: &mut Option<CiRerunDiagnostic>,
) -> Result<CiRerunStep, OrbitError> {
    let mut diagnostic = CiRerunDiagnostic::for_status(status);
    let result = plan_ci_rerun_observed(
        host,
        workspace,
        tasks,
        handoff,
        status,
        failed,
        base,
        tip,
        base_branch,
        deadline_ms,
        classified,
        &mut diagnostic,
    );
    *observation = match &result {
        Ok(CiRerunStep::Wait(reason) | CiRerunStep::Unreadable(reason)) => {
            Some(diagnostic.with_reason(reason))
        }
        Err(error) => Some(diagnostic.with_error(error)),
        _ => None,
    };
    rerun_diagnostic::record_ci_rerun_observation(
        host,
        tasks,
        execution,
        status.head_sha.as_deref(),
        observation.as_ref(),
    );
    result
}

#[allow(
    clippy::too_many_arguments,
    reason = "the observed planner shares those landing inputs and writes into the caller-owned diagnostic"
)]
fn plan_ci_rerun_observed<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    tasks: &[String],
    handoff: Option<&str>,
    status: &PullRequestStatus,
    failed: &[(Option<String>, String)],
    base: &str,
    tip: &str,
    base_branch: &str,
    deadline_ms: u64,
    classified: Option<&[super::recovery::FailedJob]>,
    diagnostic: &mut CiRerunDiagnostic,
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
    if let Some(reason) = infrastructure_backoff(host, tasks, head, &pr.to_string())? {
        return Ok(CiRerunStep::Backoff(reason));
    }
    diagnostic.stage = CiRerunStage::Jobs;
    let jobs = commit_jobs(host, workspace, head)?;
    diagnostic.bind_jobs(&jobs);
    if failed.iter().any(|check| {
        !jobs
            .iter()
            .any(|job| unsuccessful(job) && answers(job, check))
    }) {
        // External/aggregate checks keep their own authenticated evidence
        // path; a workflow sibling cannot conceal them or preempt repair.
        return Ok(CiRerunStep::NotEligible);
    }
    let runs = jobs
        .iter()
        .filter(|job| unsuccessful(job) && failed.iter().any(|check| answers(job, check)))
        .filter_map(|job| job.run_id.number())
        .collect::<BTreeSet<_>>();
    let mut conclusion_set = false;
    let mut roots = BTreeMap::<u64, Vec<CiRerunRoot>>::new();
    let mut base_jobs = None;
    let mut tip_jobs = None;
    let mut uncertain = false;
    let mut independent_candidate = false;
    let mut dependency_record = false;
    // A failed job that is neither a dependency record nor a rerun root: its
    // own class (candidate, base, inconclusive) has a route beside the records.
    let mut other_failure = false;
    if let Some((run_id, pre_roots)) =
        try_conclusion_transient(host, workspace, &jobs, &runs, deadline_ms, diagnostic)?
    {
        // Conclusions (and a pure ci-await of them) are the classification.
        // The stopped job's log is not read, so a 404 or a truncated log
        // cannot turn the set inconclusive or ineligible.
        conclusion_set = true;
        roots.insert(run_id, pre_roots);
    } else {
        for job in jobs
            .iter()
            .filter(|job| job.run_id.number().is_some_and(|run| runs.contains(&run)))
        {
            if !unsuccessful(job) {
                continue;
            }
            if super::jobs::concluded_stopped(job).is_some() {
                // Beside a real failure, a cancelled or timed-out job is not a
                // transient root, and a missing log must not hide that failure.
                continue;
            }
            let (Some(run), Some(id)) = (job.run_id.number(), job.job_id.number()) else {
                return Ok(CiRerunStep::Wait(
                    "CI retry job identity is incomplete".into(),
                ));
            };
            diagnostic.stage = CiRerunStage::Log;
            diagnostic.run_id = Some(run);
            diagnostic.job_id = Some(id);
            let log = match job_log(host, workspace, run, id, job, Some(deadline_ms)) {
                Ok(log) => log,
                Err(error) => {
                    let unknown = classified.is_some_and(|classified| {
                        classified.iter().any(|failed| {
                            failed.job_id == id && failed.class == CheckFailureClass::Unknown
                        })
                    });
                    if unknown {
                        // The landing already recorded this log as unknown.
                        // A rerun still needs the whole failed set; readable
                        // jobs keep the route their own class decides.
                        uncertain = true;
                        other_failure = true;
                        continue;
                    }
                    return Err(error);
                }
            };
            diagnostic.stage = CiRerunStage::Classification;
            let emitted_record =
                super::super::super::super::ci::log_signature::classify_log_lines(&log)
                    .into_iter()
                    .any(|(kind, line)| {
                        use super::super::super::super::ci::log_signature::{
                            LineKind, log_payload,
                        };
                        !matches!(
                            kind,
                            LineKind::RunCommand
                                | LineKind::CommandBody
                                | LineKind::ParamDump
                                | LineKind::EndGroup
                        ) && log_payload(line).starts_with("orbit-dependency-evidence:")
                    });
            if emitted_record
                && !super::suspect::cites_changed_candidate(workspace, head, base, &log)?
            {
                dependency_record = true;
                // Protocol presence grants nothing; the adapter authenticates it
                // against source, all steps, and the complete failed-job set.
                continue;
            }
            let (mut class, signature) = if let Some(prior) =
                classified.and_then(|jobs| jobs.iter().find(|job| job.job_id == id))
            {
                (prior.class, prior.signature.clone())
            } else {
                let classification = classify_logged_job(
                    host,
                    workspace,
                    job,
                    &log,
                    head,
                    base,
                    base_branch,
                    (base != tip).then_some(tip),
                    &mut base_jobs,
                    &mut tip_jobs,
                    Some(deadline_ms),
                );
                let (class, _, _, signature) = match classification {
                    Ok(classification) => classification,
                    Err(deferred) => {
                        diagnostic.bind_prerequisite(&deferred);
                        return Err(deferred.error());
                    }
                };
                (class, signature)
            };
            // Auto-fix repairs source failures on its first poll. The landing
            // rerun policy must not impose eligible-set authorization or an
            // unchanged-head repeat on that separate repair path.
            if handoff.is_none()
                && matches!(
                    class,
                    CheckFailureClass::Flaky | CheckFailureClass::BaseSuspect
                )
            {
                return Ok(CiRerunStep::NotEligible);
            }
            if class == CheckFailureClass::Flaky
                && super::suspect::has_matching_candidate_rerun(
                    host,
                    tasks,
                    head,
                    job,
                    &job_label(job),
                    &signature,
                    Some(pr),
                )
            {
                class = CheckFailureClass::Candidate;
                independent_candidate = true;
            }
            other_failure |= !matches!(
                class,
                CheckFailureClass::Flaky | CheckFailureClass::Transient
            );
            match class {
                CheckFailureClass::Flaky | CheckFailureClass::Transient => {
                    roots.entry(run).or_default().push(CiRerunRoot {
                        job_id: id,
                        check: job_label(job),
                        classification: class.as_str().into(),
                        signature,
                    })
                }
                CheckFailureClass::Candidate => {
                    // Any candidate-class sibling makes the set ineligible for a CI
                    // rerun, whether or not its log cites a changed file. A
                    // source-attributed sibling keeps its repair path; one whose log
                    // is only a generic trailer or asserted dependency record gets
                    // no infrastructure retry either.
                    independent_candidate = true;
                }
                _ => uncertain = true,
            }
        }
    }
    if !conclusion_set {
        // [ORB-20771] Only records with no other failed sibling hold here. A
        // sibling the logs cannot tie to a changed file (a guardrail such as a
        // stale body-history check) is still a candidate failure, and it takes
        // its own repair, base or stop route rather than waiting forever.
        if roots.is_empty() && dependency_record && !other_failure {
            return Ok(CiRerunStep::Wait(
                "dependency-only evidence has no independently eligible root; no candidate repair"
                    .into(),
            ));
        }
        if independent_candidate || roots.is_empty() {
            return Ok(CiRerunStep::NotEligible);
        }
        if uncertain || roots.len() != 1 || runs.len() != 1 {
            return Ok(CiRerunStep::Wait(
                "CI retry holds: the complete eligible workflow failure set is unproven".into(),
            ));
        }
    }
    let Some((run_id, roots)) = roots.into_iter().next() else {
        return Ok(CiRerunStep::NotEligible);
    };
    let request = json!({"workspace_path":workspace,"pr":pr.to_string(),"head_sha":head,"head_branch":branch,"base_branch":base_branch,"accepted_base":base,"run_id":run_id,"roots":roots,"deadline_ms":deadline_ms});
    diagnostic.stage = CiRerunStage::Authentication;
    diagnostic.run_id = Some(run_id);
    diagnostic.job_id = None;
    let response = match host.run_private_vcs_operation(operations::CI_RERUN_PLAN, request) {
        Ok(response) => response,
        Err(error) => {
            *diagnostic = diagnostic.clone().with_error(&error);
            if error
                .private_vcs_failure()
                .is_some_and(|failure| failure.read_throttle.is_some())
            {
                return Err(error);
            }
            for root in roots.iter().filter(|root| root.classification == "flaky") {
                if let Some(job) = jobs
                    .iter()
                    .find(|job| job.job_id.number() == Some(root.job_id))
                {
                    super::suspect::record_base_suspect(
                        host,
                        tasks,
                        &json!({"base":tip,"head":head,"check":root.check,"signature":root.signature,"run_id":run_id,"job_id":root.job_id,"run_attempt":job.run_attempt,"pr_number":pr,"event":job.event,"started_at":job.started_at,"completed_at":job.completed_at,"reason":"qualifying base pass; waiting for complete eligible-set authorization or a manual unchanged-head repeat"}),
                        handoff,
                    )?;
                }
            }
            return Ok(CiRerunStep::Unreadable(format!(
                "CI retry holds: the complete failed-job set was not authenticated ({error})"
            )));
        }
    };
    diagnostic.stage = CiRerunStage::PlanIdentity;
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
    diagnostic.stage = CiRerunStage::Admission;
    match host.admit_ci_rerun(
        tasks,
        handoff,
        &plan,
        super::super::super::completion_deadline::now_ms()?,
    )? {
        CiRerunAdmission::Backoff { not_before_ms } => {
            let reason = format!("infrastructure CI retry backs off until {not_before_ms} ms");
            record_retry_history(
                host,
                tasks,
                handoff,
                "ci_infrastructure_retry_backoff",
                &json!({
                    "head":plan.head_sha,"pr_number":plan.pr_number,"run_id":plan.run_id,
                    "run_attempt":plan.run_attempt,"classification":"transient",
                    "not_before_ms":not_before_ms,"reason":reason,
                })
                .to_string(),
            )?;
            Ok(CiRerunStep::Backoff(reason))
        }
        CiRerunAdmission::InfrastructureExhausted { attempts } => {
            let reason = format!(
                "ci_infrastructure_retry_exhausted: classification=transient attempts={attempts} limit={}; the candidate stays in review",
                host.pr_config().ci_infrastructure_retry.max_attempts
            );
            record_retry_exhausted(host, tasks, handoff, &plan, attempts, &reason)?;
            Ok(CiRerunStep::BudgetSpent(reason))
        }
        CiRerunAdmission::Refused(reason) => {
            if conclusion_set && reason == SHARED_RETRY_BUDGET_SPENT {
                let stop = cancellation_timeout_reason(&plan.roots);
                record_cancellation_exhausted(host, tasks, handoff, &stop)?;
                Ok(CiRerunStep::BudgetSpent(stop))
            } else {
                Ok(CiRerunStep::Wait(format!("CI retry holds: {reason}")))
            }
        }
        CiRerunAdmission::Observe(previous) => {
            // Same attempt: the send is in flight. A later attempt that is
            // still a conclusion stop has used the one rerun.
            if conclusion_set && plan.run_attempt > previous.run_attempt {
                let stop = cancellation_timeout_reason(&plan.roots);
                record_cancellation_exhausted(host, tasks, handoff, &stop)?;
                return Ok(CiRerunStep::BudgetSpent(stop));
            }
            Ok(CiRerunStep::Wait(format!(
                "CI retry was reserved for run {} attempt {} on head {}; observing without resending, including after an unknown reply",
                previous.run_id, previous.run_attempt, previous.head_sha
            )))
        }
        CiRerunAdmission::Reserved => {
            if !send_reserved_rerun(host, workspace, tasks, handoff, pr, &plan, deadline_ms)? {
                return Ok(CiRerunStep::Wait(
                    "CI retry reservation or current owner authority changed before send".into(),
                ));
            }
            Ok(CiRerunStep::Acted(Box::new(plan)))
        }
    }
}

/// Send a just-reserved failed-jobs retry once and record it. `false` when
/// the reservation or the owner's authority changed before the send.
fn send_reserved_rerun<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    tasks: &[String],
    handoff: Option<&str>,
    pr: u64,
    plan: &CiRerunPlan,
    deadline_ms: u64,
) -> Result<bool, OrbitError> {
    // The exact set and budget are durable now. A lost/error reply is
    // ambiguous; never retransmit. The next poll reads actual checks.
    if !matches!(host.admit_ci_rerun(tasks,handoff,plan,super::super::super::completion_deadline::now_ms()?)?,CiRerunAdmission::Observe(previous) if *previous==*plan)
    {
        return Ok(false);
    }
    let run_id = plan.run_id;
    let reply=host.run_private_vcs_operation(operations::CI_RERUN_FAILED,json!({"workspace_path":workspace,"run_id":run_id.to_string(),"plan":plan,"deadline_ms":deadline_ms}));
    if reply.is_ok() {
        let jobs=plan.roots.iter().map(|root| {
            let original=plan.failed_jobs.iter().find(|job|job["id"].as_u64()==Some(root.job_id));
            json!({"job_id":root.job_id,"check":root.check,"classification":root.classification,"signature":root.signature,"run_attempt":plan.run_attempt,"started_at":original.map(|job|job["started_at"].clone()),"completed_at":original.map(|job|job["completed_at"].clone())})
        }).collect::<Vec<_>>();
        let note=json!({"head":plan.head_sha,"pr_number":pr,"run_id":plan.run_id,"event":"pull_request","jobs":jobs}).to_string();
        if let Some(handoff) = handoff {
            host.record_handoff_landing_history(handoff, "ci_check_rerun", &note)?;
        } else {
            for task in tasks {
                host.apply_task_automation_update(
                    task,
                    crate::context::TaskAutomationUpdate {
                        append_history: vec![orbit_types::task::TaskHistoryEntry {
                            at: chrono::Utc::now(),
                            by: "system".into(),
                            event: "ci_check_rerun".into(),
                            note: Some(note.clone()),
                            from_status: None,
                            to_status: None,
                            operation_id: None,
                            decision: None,
                        }],
                        ..Default::default()
                    },
                )?;
            }
        }
    }
    Ok(true)
}

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
pub(super) fn request_classification_rerun<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    tasks: &[String],
    handoff: Option<&str>,
    status: &PullRequestStatus,
    base: &str,
    base_branch: &str,
    deadline_ms: u64,
    failed: &[super::recovery::FailedJob],
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
        if super::jobs::concluded_stopped(job).is_some() {
            let Some(root) = conclusion_root(job) else {
                return Ok(CiRerunStep::Wait(
                    "CI retry job identity is incomplete".into(),
                ));
            };
            roots.push(root);
            continue;
        }
        let log = job_log(host, workspace, run_id, id, job, Some(deadline_ms))?;
        let signature =
            super::super::super::super::ci::log_signature::error_signature(&log, failed_step(job));
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

/// The admission sentence for a head that already used its one CI retry.
const SHARED_RETRY_BUDGET_SPENT: &str = "the shared unchanged-head CI retry budget is spent";

/// Jobs in `runs` whose conclusions alone are a transient stop, including a
/// failure job that only reports awaiting that stop. `None` keeps today's
/// log classification. A failure-job log that cannot be read is still an error.
fn try_conclusion_transient<H: RuntimeHost + ?Sized>(
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
        if super::jobs::concluded_stopped(job).is_some() {
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
        let Some(signal) = super::jobs::exclusive_awaited_stop(&log) else {
            return Ok(None);
        };
        let signature = super::super::super::super::ci::log_signature::error_signature(
            &log,
            super::jobs::failed_step(job),
        );
        if signature.step_fallback {
            return Ok(None);
        }
        match signal {
            super::jobs::AwaitedStop::Timeout => saw_timeout = true,
            super::jobs::AwaitedStop::Cancelled => needs_cancelled = true,
            super::jobs::AwaitedStop::TimedOut => needs_timed_out = true,
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

fn conclusion_root(job: &CiCommitJobEvidence) -> Option<CiRerunRoot> {
    let conclusion = super::jobs::concluded_stopped(job)?;
    Some(CiRerunRoot {
        job_id: job.job_id.number()?,
        check: job_label(job),
        classification: CheckFailureClass::Transient.as_str().into(),
        signature: super::jobs::conclusion_signature(conclusion),
    })
}

fn cancellation_timeout_reason(roots: &[CiRerunRoot]) -> String {
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

fn record_cancellation_exhausted<H: RuntimeHost + ?Sized>(
    host: &H,
    tasks: &[String],
    handoff: Option<&str>,
    reason: &str,
) -> Result<(), OrbitError> {
    if let Some(handoff) = handoff {
        host.record_handoff_landing_history(handoff, "ci_check_rerun_exhausted", reason)?;
        return Ok(());
    }
    for task in tasks {
        host.apply_task_automation_update(
            task,
            crate::context::TaskAutomationUpdate {
                append_history: vec![orbit_types::task::TaskHistoryEntry {
                    at: chrono::Utc::now(),
                    by: "system".into(),
                    event: "ci_check_rerun_exhausted".into(),
                    note: Some(reason.to_string()),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                }],
                ..Default::default()
            },
        )?;
    }
    Ok(())
}

/// Read only the durable ledger while backing off; do not re-fetch completed
/// jobs, logs or provider authentication on every completion poll.
pub(super) fn infrastructure_backoff<H: RuntimeHost + ?Sized>(
    host: &H,
    tasks: &[String],
    head: &str,
    pr: &str,
) -> Result<Option<String>, OrbitError> {
    let Some(task) = tasks.iter().min() else {
        return Ok(None);
    };
    let history = host.get_task_history(task)?;
    let now = super::super::super::completion_deadline::now_ms()?;
    for entry in history
        .iter()
        .rev()
        .filter(|entry| entry.event == "ci_infrastructure_retry_backoff")
    {
        let record: serde_json::Value =
            serde_json::from_str(entry.note.as_deref().unwrap_or_default()).map_err(|error| {
                OrbitError::Execution(format!("unreadable CI retry backoff: {error}"))
            })?;
        if record["head"] == head && record["pr_number"] == pr {
            let not_before = record["not_before_ms"].as_u64().ok_or_else(|| {
                OrbitError::Execution("CI retry backoff has no not-before".into())
            })?;
            return Ok((now < not_before)
                .then(|| format!("infrastructure CI retry backs off until {not_before} ms")));
        }
    }
    Ok(None)
}

fn record_retry_exhausted<H: RuntimeHost + ?Sized>(
    host: &H,
    tasks: &[String],
    handoff: Option<&str>,
    plan: &CiRerunPlan,
    attempts: u32,
    reason: &str,
) -> Result<(), OrbitError> {
    let event = "ci_infrastructure_retry_exhausted";
    let note = json!({"head":plan.head_sha,"pr_number":plan.pr_number,
        "classification":"transient","attempts":attempts,"reason":reason})
    .to_string();
    record_retry_history(host, tasks, handoff, event, &note)
}

fn record_retry_history<H: RuntimeHost + ?Sized>(
    host: &H,
    tasks: &[String],
    handoff: Option<&str>,
    event: &str,
    note: &str,
) -> Result<(), OrbitError> {
    if let Some(handoff) = handoff {
        host.record_handoff_landing_history(handoff, event, note)?;
    } else {
        for task in tasks {
            if host
                .get_task_history(task)?
                .iter()
                .any(|entry| entry.event == event && entry.note.as_deref() == Some(note))
            {
                continue;
            }
            host.apply_task_automation_update(
                task,
                crate::context::TaskAutomationUpdate {
                    append_history: vec![orbit_types::task::TaskHistoryEntry {
                        at: chrono::Utc::now(),
                        by: "system".into(),
                        event: event.into(),
                        note: Some(note.to_string()),
                        from_status: None,
                        to_status: None,
                        operation_id: None,
                        decision: None,
                    }],
                    ..Default::default()
                },
            )?;
        }
    }
    Ok(())
}
