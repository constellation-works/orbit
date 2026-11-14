//! The second phase of failed-check recovery: plan the rerun, then route the
//! deciding class to a rerun, a branch update, a repair, a requeue or a stay.

use std::cell::OnceCell;
use std::ops::ControlFlow;

use orbit_common::OrbitError;
use serde_json::json;

use crate::context::{HandoffLandingContext, RuntimeHost};

use super::super::super::super::operations;
use super::super::super::super::pr::PullRequestStatus;

use super::super::base_update::contains;
use super::super::head::onto_passing_tip;
use super::super::jobs::describe;
use super::classify::{ClassifiedFailures, classify_failed_checks, task_history};
use super::{CheckFailureClass, CheckRecovery, CheckRecoveryStep, unreadable_job_text};

/// What a candidate-caused failure does once the automatic repair is spent.
const REQUEUE_OUTCOME: &str = "the automatic repair is already spent, so the owner ends the claim \
     and returns the task to the backlog, where its next run repairs this candidate";

/// Classify the failed checks on `status`'s head and act on them.
///
/// `reason` is the failure as the landing words it. `landing_tip` is the
/// landing branch's current commit in the owner checkout.
pub(in crate::executor::automation::vcs) fn recover_failed_checks<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffLandingContext,
    status: &PullRequestStatus,
    failed: &[(Option<String>, String)],
    landing_tip: Option<&str>,
    recovery: &mut CheckRecovery,
    reason: &str,
) -> Result<CheckRecoveryStep, OrbitError> {
    let workspace = &context.workspace_path;
    let (Some(head), Some(pr)) = (status.head_sha.as_deref(), status.number) else {
        return Ok(CheckRecoveryStep::Stop(format!(
            "{reason}; the failure was not classified: the pull request reported no head or \
             number"
        )));
    };
    let unclassified = |why: String| {
        CheckRecoveryStep::HoldInReview(format!(
            "{reason}; the failure was not classified ({why}), so the candidate stays in review"
        ))
    };
    let history_cell = OnceCell::new();
    let ClassifiedFailures {
        moved_tip,
        jobs,
        answered_checks,
        code,
        has_aggregates,
    } = match classify_failed_checks(
        host,
        context,
        failed,
        landing_tip,
        recovery,
        head,
        pr,
        &unclassified,
        &history_cell,
    )? {
        ControlFlow::Break(step) => return Ok(step),
        ControlFlow::Continue(classified) => classified,
    };
    let history = || task_history(&history_cell, host, &context.task_id);
    let planned = if has_aggregates {
        Ok(super::super::rerun::CiRerunStep::NotEligible)
    } else {
        super::super::rerun::plan_ci_rerun(
            host,
            workspace,
            std::slice::from_ref(&context.task_id),
            Some(&context.handoff_id),
            status,
            failed,
            &context.candidate.base.commit,
            landing_tip.unwrap_or(&context.candidate.base.commit),
            &context.candidate.base_branch,
            recovery.deadline_ms,
            Some(&jobs),
            &context.handoff_id,
            &mut recovery.rerun_diagnostic,
        )
    };
    let planned = planned.unwrap_or_else(|error| {
        let reason = format!("CI retry evidence is unavailable: {error}");
        // A provider throttle names when the same read may repeat; it is a
        // paced hold, not a failed read.
        if error
            .private_vcs_failure()
            .is_some_and(|failure| failure.read_throttle.is_some())
        {
            super::super::rerun::CiRerunStep::Wait(reason)
        } else {
            super::super::rerun::CiRerunStep::Unreadable(reason)
        }
    });
    let mut unreadable_retry = None;
    if let super::super::rerun::CiRerunStep::Unreadable(reason) = &planned {
        let cycles = recovery
            .unreadable_rerun_cycles
            .entry(head.to_owned())
            .or_default();
        *cycles = cycles.saturating_add(1);
        if *cycles >= recovery.max_base_ci_cycles {
            unreadable_retry = Some((*cycles, reason.clone()));
        }
    } else {
        recovery.unreadable_rerun_cycles.remove(head);
    }
    match planned {
        super::super::rerun::CiRerunStep::Backoff(reason)
        | super::super::rerun::CiRerunStep::Wait(reason) => {
            return Ok(CheckRecoveryStep::WaitForBase(reason));
        }
        super::super::rerun::CiRerunStep::Unreadable(reason) if unreadable_retry.is_none() => {
            return Ok(CheckRecoveryStep::WaitForBase(reason));
        }
        super::super::rerun::CiRerunStep::Unreadable(_) => {}
        super::super::rerun::CiRerunStep::Acted(plan) => {
            recovery.rerun = true;
            recovery
                .handled_checks
                .extend(answered_checks.iter().cloned());
            recovery.actions.push(format!(
                "reserved unchanged-head failed-jobs retry for run {} attempt {}",
                plan.run_id, plan.run_attempt
            ));
            return Ok(CheckRecoveryStep::Acted);
        }
        super::super::rerun::CiRerunStep::BudgetSpent(reason) => {
            return Ok(CheckRecoveryStep::Stop(format!(
                "{reason}; {}",
                describe(
                    jobs.iter()
                        .map(|job| job.class)
                        .max()
                        .unwrap_or(CheckFailureClass::Candidate),
                    &jobs,
                )
            )));
        }
        super::super::rerun::CiRerunStep::NotEligible => {}
    }

    let class = jobs
        .iter()
        .map(|job| job.class)
        .max()
        .unwrap_or(CheckFailureClass::Candidate);
    let classified = describe(class, &jobs);
    let evidence = |outcome: &str| format!("{reason}; {classified}; {outcome}");

    // [ORB-21586] The unchanged-head retry stayed unreadable for the cycle
    // limit. A same-check pass on the base already shows the failure is the
    // candidate's own, so it goes to the repair path; any other class that
    // needed the retry's evidence stops naming the read failure. A candidate
    // or inherited failure keeps its own route below.
    if let Some((cycles, read_failure)) = unreadable_retry {
        let limit = recovery.max_base_ci_cycles;
        match class {
            CheckFailureClass::Flaky if context.automatic_repair_spent => {
                return Ok(CheckRecoveryStep::Requeue(evidence(&format!(
                    "a successful same-check run on the base shows the failure is the candidate's own, and the unchanged-head retry could not be read after {cycles} observation cycles (limit {limit}): {read_failure}; {REQUEUE_OUTCOME}"
                ))));
            }
            CheckFailureClass::Flaky => {
                return Ok(CheckRecoveryStep::Repair(evidence(&format!(
                    "a successful same-check run on the base shows the failure is the candidate's own, and the unchanged-head retry could not be read after {cycles} observation cycles (limit {limit}): {read_failure}; the candidate is handed to one automatic repair"
                ))));
            }
            CheckFailureClass::Candidate | CheckFailureClass::Inherited => {}
            _ => {
                return Ok(CheckRecoveryStep::Stop(evidence(&format!(
                    "its CI retry evidence could not be read after {cycles} observation cycles (limit {limit}): {read_failure}; nothing is rerun, updated or repaired and the candidate stays in review"
                ))));
            }
        }
    }

    // [model ranking §5.1] A candidate set routed to repair with no evidence
    // of its cause (no job cites a changed file, none is the base's or the
    // infrastructure's, every log was read), on a head that has had no
    // rerun, is rerun once first: the second result classes it as flaky or
    // the head's own. Unreadable evidence keeps its stop and spends nothing.
    if class == CheckFailureClass::Candidate
        && !has_aggregates
        && !jobs.iter().any(|job| {
            code.contains_key(&job.job_id)
                || matches!(
                    job.class,
                    CheckFailureClass::Transient
                        | CheckFailureClass::Inherited
                        | CheckFailureClass::Unknown
                        | CheckFailureClass::Inconclusive
                )
        })
        && !super::super::failure_ledger::head_was_rerun(history(), head)
    {
        match super::super::rerun::request_classification_rerun(
            host,
            workspace,
            std::slice::from_ref(&context.task_id),
            Some(&context.handoff_id),
            status,
            &context.candidate.base.commit,
            &context.candidate.base_branch,
            recovery.deadline_ms,
            &jobs,
        ) {
            Ok(Some(plan)) => {
                recovery.rerun = true;
                recovery
                    .handled_checks
                    .extend(answered_checks.iter().cloned());
                recovery.actions.push(format!(
                    "reran run {} attempt {} on the unchanged head to establish the failure's cause: {classified}",
                    plan.run_id, plan.run_attempt
                ));
                return Ok(CheckRecoveryStep::Acted);
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, head, "CI failure ledger: the classification rerun was not sent; the landing keeps its route");
            }
        }
    }

    match class {
        CheckFailureClass::BaseSuspect => {
            let limit = recovery.max_base_ci_cycles;
            let mut exhausted = None;
            for job in jobs
                .iter()
                .filter(|job| job.class == CheckFailureClass::BaseSuspect)
            {
                let key = (head.to_owned(), job.label.clone(), job.signature.clone());
                let cycles = recovery.base_suspect_cycles.entry(key).or_default();
                *cycles = cycles.saturating_add(1);
                if *cycles >= limit {
                    exhausted.get_or_insert((*cycles, job.label.clone(), job.signature.clone()));
                }
            }
            if let Some((cycles, check, signature)) = exhausted {
                Ok(CheckRecoveryStep::HoldForBase(evidence(&format!(
                    "escalated after {cycles} base CI observation cycles (limit {limit}): check '{check}' still fails with signature `{signature}` on unchanged head {head}; a matching pull_request rerun failure and a successful same-check agent-main run on the accepted base ancestry have not both been observed, so review the missing base/check evidence",
                ))))
            } else {
                Ok(CheckRecoveryStep::WaitForBase(evidence(
                    "waiting for a same-signature pull_request rerun and a successful same-check agent-main run on the accepted base ancestry; no candidate repair",
                )))
            }
        }
        CheckFailureClass::Transient => Ok(CheckRecoveryStep::HoldInReview(evidence(
            "it failed again after its one rerun, so the candidate stays in review",
        ))),
        CheckFailureClass::Inherited
            if (recovery.updated || head != context.candidate.candidate.commit)
                && onto_passing_tip(&jobs)
                && landing_tip.is_some_and(|tip| contains(workspace, tip, head)) =>
        {
            // An update already landed the candidate on a base tip where
            // the same job passes, so the failure that remains is the
            // candidate's. It takes the same single repair a candidate-class
            // failure does, and a spent repair requeues with the run held.
            if context.automatic_repair_spent {
                Ok(CheckRecoveryStep::Requeue(evidence(REQUEUE_OUTCOME)))
            } else {
                Ok(CheckRecoveryStep::Repair(evidence(
                    "it failed again after a branch update onto a base where the same job \
                     passes, so the candidate is handed to one automatic repair",
                )))
            }
        }
        CheckFailureClass::Inherited => {
            let Some(tip) = moved_tip.filter(|tip| !contains(workspace, tip, head)) else {
                return Ok(CheckRecoveryStep::WaitForBase(evidence(
                    "waiting for a newer passing base commit within the landing deadline; the base is still red",
                )));
            };
            // After the first update, only a same-job passing base authorizes
            // another. A newer red or unproven tip keeps waiting.
            if (recovery.updated || head != context.candidate.candidate.commit)
                && !onto_passing_tip(&jobs)
            {
                return Ok(CheckRecoveryStep::WaitForBase(evidence(
                    "waiting for the same job to pass on a newer base commit within the landing deadline",
                )));
            }
            let request = orbit_types::workflow::PrBaseUpdateReservation {
                execution: context.handoff_id.clone(),
                repository: context.candidate.repository.clone(),
                pr_number: pr.to_string(),
                head_sha: head.into(),
                head_branch: context.candidate.source_branch.clone(),
                base_branch: context.candidate.base_branch.clone(),
                base_sha: tip.into(),
                deadline_ms: recovery.deadline_ms,
                evidence: json!({"cause":"inherited","handoff_id":context.handoff_id,"candidate":context.candidate.candidate.commit,"classification":classified}),
            };
            match host.admit_handoff_base_update(&context.handoff_id, &request)? {
                orbit_types::workflow::PrBaseUpdateAdmission::Reserved => {}
                orbit_types::workflow::PrBaseUpdateAdmission::Observe(previous) => {
                    super::super::super::super::completion_deadline::Deadline(previous.deadline_ms)
                        .remaining()?;
                    return Ok(CheckRecoveryStep::Wait);
                }
                // A spent budget waits out the deadline for the base itself to
                // recover; unreadable or invalid reservation evidence stops.
                orbit_types::workflow::PrBaseUpdateAdmission::Refused(reason)
                    if reason.starts_with(orbit_types::workflow::PR_BASE_UPDATE_BUDGET_SPENT) =>
                {
                    return Ok(CheckRecoveryStep::WaitForBase(evidence(&format!(
                        "waiting within the landing deadline: {reason}"
                    ))));
                }
                orbit_types::workflow::PrBaseUpdateAdmission::Refused(reason) => {
                    return Ok(CheckRecoveryStep::Stop(reason));
                }
            }
            // The reservation survives an unknown provider reply. Re-observe
            // its actual head; never repeat the expected-head update.
            let _reply = host.run_private_vcs_operation(
                operations::PR_UPDATE_BRANCH,
                json!({
                    "pr": pr.to_string(),
                    "expected_head_sha": head,
                    "workspace_path": workspace.to_string_lossy(),
                }),
            );
            recovery.updated = true;
            recovery.handled.extend(jobs.iter().map(|job| job.job_id));
            recovery.handled_checks.extend(answered_checks);
            recovery.actions.push(format!(
                "merged base tip {tip} into the pull request's branch: {classified}"
            ));
            Ok(CheckRecoveryStep::Acted)
        }
        CheckFailureClass::Candidate if context.automatic_repair_spent => {
            Ok(CheckRecoveryStep::Requeue(evidence(REQUEUE_OUTCOME)))
        }
        CheckFailureClass::Flaky => Ok(CheckRecoveryStep::WaitForBase(evidence(
            "the unchanged-head retry has no authenticated repeated signature; no candidate repair",
        ))),
        CheckFailureClass::Candidate => Ok(CheckRecoveryStep::Requeue(evidence(
            "the owner ends the claim and returns the task to the backlog, where its next run repairs this candidate",
        ))),
        CheckFailureClass::Inconclusive => Ok(CheckRecoveryStep::HoldInReview(evidence(
            "its evidence could not be read in full, so nothing is rerun, updated or repaired \
             and the candidate stays in review",
        ))),
        // Reachable only when every job is unknown. The early return above
        // is the same stop; this arm keeps the match exhaustive.
        CheckFailureClass::Unknown => Ok(unclassified(unreadable_job_text(&jobs))),
    }
}
