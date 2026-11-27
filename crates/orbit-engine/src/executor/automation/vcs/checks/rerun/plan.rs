//! Planning and admitting the unchanged-head failed-jobs retry.

use super::super::super::pr_answers::PullRequestStatus;
use super::super::rerun_diagnostic::{self, CiRerunDiagnostic, CiRerunStage};
use super::super::{
    head::{BaseComparison, classify_logged_job},
    jobs::{answers, commit_jobs, job_label, unsuccessful},
    log::job_log,
    recovery::CheckFailureClass,
};
use super::conclusion::{cancellation_timeout_reason, handoff_step_set, try_conclusion_transient};
use super::send::{
    infrastructure_backoff, record_cancellation_exhausted, record_retry_exhausted,
    record_retry_history, send_reserved_rerun,
};
use super::unreadable_log::{
    UNREADABLE_LOG_RERUN_PENDING, UnreadableLogRetry, retry_unreadable_log,
    unreadable_log_rerun_pending,
};
use super::{
    CiRerunStep, PlanMismatch, RerunPlanRequest, authenticated_plan, is_no_authenticated_plan_error,
};
use crate::context::RuntimeHost;
use orbit_common::OrbitError;
use orbit_types::workflow::{
    CI_INFRASTRUCTURE_RETRY_BACKOFF_EVENT, CiBaseSuspectLegacy, CiBaseSuspectRecord,
    CiInfrastructureRetryBackoffRecord, CiRerunAdmission, CiRerunRefusal, CiRerunRoot,
    PrAutoFixBaseRed,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

/// What one unchanged-head retry plan reads: the owner tasks or handoff,
/// the pull status and its failed checks, the candidate's base, the base
/// branch and its tip, and the jobs a landing already classified.
#[derive(Clone, Copy)]
pub(in crate::executor::automation::vcs) struct CiRerunPlanInputs<'a> {
    pub(in crate::executor::automation::vcs) workspace: &'a Path,
    pub(in crate::executor::automation::vcs) tasks: &'a [String],
    pub(in crate::executor::automation::vcs) handoff: Option<&'a str>,
    pub(in crate::executor::automation::vcs) status: &'a PullRequestStatus,
    pub(in crate::executor::automation::vcs) failed: &'a [(Option<String>, String)],
    pub(in crate::executor::automation::vcs) base: &'a str,
    pub(in crate::executor::automation::vcs) tip: &'a str,
    pub(in crate::executor::automation::vcs) base_branch: &'a str,
    pub(in crate::executor::automation::vcs) deadline_ms: u64,
    pub(in crate::executor::automation::vcs) classified:
        Option<&'a [crate::executor::automation::vcs::checks::recovery::FailedJob]>,
    pub(in crate::executor::automation::vcs) allow_unreadable_log_rerun: bool,
}

pub(in crate::executor::automation::vcs) fn plan_ci_rerun<H: RuntimeHost + ?Sized>(
    host: &H,
    inputs: CiRerunPlanInputs<'_>,
    execution: &str,
    observation: &mut Option<CiRerunDiagnostic>,
) -> Result<CiRerunStep, OrbitError> {
    let CiRerunPlanInputs { tasks, status, .. } = inputs;
    let mut diagnostic = CiRerunDiagnostic::for_status(status);
    let result = plan_ci_rerun_observed(host, inputs, &mut diagnostic);
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

fn plan_ci_rerun_observed<H: RuntimeHost + ?Sized>(
    host: &H,
    inputs: CiRerunPlanInputs<'_>,
    diagnostic: &mut CiRerunDiagnostic,
) -> Result<CiRerunStep, OrbitError> {
    let CiRerunPlanInputs {
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
        allow_unreadable_log_rerun,
    } = inputs;
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
    let owner_history = allow_unreadable_log_rerun
        .then(|| tasks.iter().min())
        .flatten()
        .and_then(|task| host.get_task_history(task).ok());
    if owner_history.as_ref().is_some_and(|history| {
        unreadable_log_rerun_pending(history, &pr.to_string(), head, &jobs, &runs)
    }) {
        return Ok(CiRerunStep::Wait(UNREADABLE_LOG_RERUN_PENDING.into()));
    }
    let recover_unreadable_log = |diagnostic: &CiRerunDiagnostic, error| {
        if matches!(error, OrbitError::Execution(_))
            && diagnostic.stage == CiRerunStage::Log
            && runs.len() == 1
            && owner_history.as_ref().is_some_and(|history| {
                !history
                    .iter()
                    .filter_map(PrAutoFixBaseRed::from_entry)
                    .any(|record| record.failed_head == head)
            })
            && let Some(job) = jobs.iter().find(|job| {
                job.run_id.number() == diagnostic.run_id && job.job_id.number() == diagnostic.job_id
            })
        {
            match retry_unreadable_log(
                host,
                UnreadableLogRetry {
                    workspace,
                    tasks,
                    pr,
                    head,
                    head_branch: branch,
                    accepted_base: base,
                    base_branch,
                    deadline_ms,
                },
                &jobs,
                failed,
                job,
            ) {
                Ok(Some(step)) => return Ok(step),
                Ok(None) => {}
                Err(retry_error) => {
                    tracing::debug!(%retry_error, "unreadable job log rerun could not be authorized");
                }
            }
        }
        Err(error)
    };
    let mut conclusion_set = false;
    let mut roots = BTreeMap::<u64, Vec<CiRerunRoot>>::new();
    let mut comparison = BaseComparison {
        host,
        workspace,
        head,
        base,
        base_branch,
        moved_tip: (base != tip).then_some(tip),
        deadline_ms: Some(deadline_ms),
        base_jobs: None,
        tip_jobs: None,
    };
    let mut uncertain = false;
    let mut independent_candidate = false;
    let mut dependency_record = false;
    // A failed job that is neither a dependency record nor a rerun root: its
    // own class (candidate, base, inconclusive) has a route beside the records.
    let mut other_failure = false;
    // [ORB-23813] Every failed job stopped in a declared dependency-handoff
    // step: the failed-step names are the classification, so no log is read.
    let handoff_set = handoff_step_set(
        &host.run_settings().pr_config.ci_handoff_steps,
        &jobs,
        failed,
    );
    let roots_preclassified = handoff_set.is_some();
    let conclusion_roots = if roots_preclassified {
        None
    } else {
        match try_conclusion_transient(host, workspace, &jobs, &runs, deadline_ms, diagnostic) {
            Ok(roots) => roots,
            Err(error) => return recover_unreadable_log(diagnostic, error),
        }
    };
    if let Some((run_id, handoff_roots)) = handoff_set {
        roots.insert(run_id, handoff_roots);
    } else if let Some((run_id, pre_roots)) = conclusion_roots {
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
            if super::super::jobs::concluded_stopped(job).is_some() {
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
                    return recover_unreadable_log(diagnostic, error);
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
                && !super::super::suspect::cites_changed_candidate(
                    &host.process_env(),
                    workspace,
                    head,
                    base,
                    &log,
                )?
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
                let classification = classify_logged_job(&mut comparison, job, &log);
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
                && super::super::suspect::has_matching_candidate_rerun(
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
    if !conclusion_set && !roots_preclassified {
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
    diagnostic.stage = CiRerunStage::Authentication;
    diagnostic.run_id = Some(run_id);
    diagnostic.job_id = None;
    let plan = match authenticated_plan(host, &request) {
        Ok(Ok(plan)) => plan,
        Ok(Err(PlanMismatch)) => {
            diagnostic.stage = CiRerunStage::PlanIdentity;
            return Ok(CiRerunStep::Wait(
                "CI retry plan differs from the exact classified request".into(),
            ));
        }
        Err(error) => {
            if is_no_authenticated_plan_error(&error) {
                diagnostic.stage = CiRerunStage::PlanIdentity;
                return Err(error);
            }
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
                    super::super::suspect::record_base_suspect(
                        host,
                        tasks,
                        &CiBaseSuspectRecord {
                            base: Some(tip.to_string()),
                            head: Some(head.to_string()),
                            check: Some(root.check.clone()),
                            signature: Some(root.signature.clone()),
                            run_id: CiBaseSuspectLegacy::Known(run_id),
                            job_id: CiBaseSuspectLegacy::Known(root.job_id),
                            run_attempt: super::super::suspect::base_suspect_legacy_number(
                                &job.run_attempt,
                            ),
                            pr_number: Some(pr),
                            event: super::super::suspect::base_suspect_legacy_event(&job.event),
                            started_at: super::super::suspect::base_suspect_legacy_text(
                                &job.started_at,
                            ),
                            completed_at: super::super::suspect::base_suspect_legacy_text(
                                &job.completed_at,
                            ),
                            reason: Some("qualifying base pass; waiting for complete eligible-set authorization or a manual unchanged-head repeat".to_string()),
                            legacy_pull_requests: Vec::new(),
                        },
                        handoff,
                    )?;
                }
            }
            return Ok(CiRerunStep::Unreadable(format!(
                "CI retry holds: the complete failed-job set was not authenticated ({error})"
            )));
        }
    };
    diagnostic.stage = CiRerunStage::Admission;
    match host.admit_ci_rerun(
        tasks,
        handoff,
        &plan,
        super::super::super::completion_deadline::now_ms()?,
    )? {
        CiRerunAdmission::Backoff { not_before_ms } => {
            let reason = format!("infrastructure CI retry backs off until {not_before_ms} ms");
            let note =
                CiInfrastructureRetryBackoffRecord::from_plan(&plan, not_before_ms, reason.clone())
                    .to_history_note()
                    .map_err(|error| OrbitError::Execution(error.to_string()))?;
            record_retry_history(
                host,
                tasks,
                handoff,
                CI_INFRASTRUCTURE_RETRY_BACKOFF_EVENT,
                &note,
            )?;
            Ok(CiRerunStep::Backoff(reason))
        }
        CiRerunAdmission::InfrastructureExhausted { attempts } => {
            let reason = format!(
                "ci_infrastructure_retry_exhausted: classification=transient attempts={attempts} limit={}; the candidate stays in review",
                host.run_settings()
                    .pr_config
                    .ci_infrastructure_retry
                    .max_attempts
            );
            record_retry_exhausted(host, tasks, handoff, &plan, attempts, &reason)?;
            Ok(CiRerunStep::BudgetSpent(reason))
        }
        CiRerunAdmission::Refused(CiRerunRefusal::SharedBudgetSpent) if conclusion_set => {
            let stop = cancellation_timeout_reason(&plan.roots);
            record_cancellation_exhausted(host, tasks, handoff, &stop)?;
            Ok(CiRerunStep::BudgetSpent(stop))
        }
        CiRerunAdmission::Refused(
            reason @ (CiRerunRefusal::InvalidAuthorization
            | CiRerunRefusal::UnreadableReservation
            | CiRerunRefusal::UnsupportedVersion
            | CiRerunRefusal::ConflictingReservations
            | CiRerunRefusal::UnreadableLegacyHistory
            | CiRerunRefusal::MissingLegacyActionIdentity
            | CiRerunRefusal::SharedBudgetSpent
            | CiRerunRefusal::RequiresOwningHost
            | CiRerunRefusal::OwningTaskTerminal
            | CiRerunRefusal::LedgerAbsent),
        ) => Ok(CiRerunStep::Wait(format!("CI retry holds: {reason}"))),
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
