//! The first phase of failed-check recovery: read and classify each failed
//! job, and each failed required check no job answers, on the pull request's
//! head.

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;

use orbit_common::OrbitError;
use orbit_types::task::TaskHistoryEntry;
use serde_json::json;

use crate::context::{HandoffLandingContext, RuntimeHost};

use super::super::super::super::operations::{CiCommitJobEvidence, CiJobEvent};

use super::super::annotations::unanswered_required;
use super::super::head::classify_logged_job;
use super::super::jobs::{
    answers, commit_jobs, dependency_only_log, excerpt, job_label, unsuccessful,
};
use super::super::log::job_log;
use super::super::read_wait::ThrottledRead;
use super::dependency::{DependencyAuthScope, fold_dependency_waiters};
use super::{CheckFailureClass, CheckRecovery, CheckRecoveryStep, FailedJob, unreadable_job_text};

/// The failed jobs the first phase classified, for the second phase to act on.
pub(super) struct ClassifiedFailures<'a> {
    /// The landing branch's commit when it moved past the candidate's base.
    pub(super) moved_tip: Option<&'a str>,
    pub(super) jobs: Vec<FailedJob>,
    pub(super) answered_checks: BTreeSet<(Option<String>, String)>,
    /// [model ranking §5.1] Evidence that a job's failure is the head's own,
    /// by job id.
    pub(super) code: BTreeMap<u64, String>,
    pub(super) has_aggregates: bool,
}

/// Classify the failed checks on `head`. `Break` is a step the landing takes
/// before any rerun is planned; `Continue` carries the classified jobs.
#[allow(
    clippy::too_many_arguments,
    reason = "the classification phase shares the landing's inputs with the entry point"
)]
pub(super) fn classify_failed_checks<'a, H: RuntimeHost + ?Sized>(
    host: &H,
    context: &'a HandoffLandingContext,
    failed: &[(Option<String>, String)],
    landing_tip: Option<&'a str>,
    recovery: &mut CheckRecovery,
    head: &str,
    pr: u64,
    unclassified: &dyn Fn(String) -> CheckRecoveryStep,
    history_cell: &OnceCell<Vec<TaskHistoryEntry>>,
) -> Result<ControlFlow<CheckRecoveryStep, ClassifiedFailures<'a>>, OrbitError> {
    let workspace = &context.workspace_path;
    if let Some(reason) = super::super::rerun::infrastructure_backoff(
        host,
        std::slice::from_ref(&context.task_id),
        head,
        &pr.to_string(),
    )? {
        return Ok(ControlFlow::Break(CheckRecoveryStep::WaitForBase(reason)));
    }
    let head_jobs = match commit_jobs(host, workspace, head) {
        Ok(jobs) => jobs,
        Err(error) => {
            let reason = format!("its jobs could not be read: {error}");
            return Ok(ControlFlow::Break(
                match ThrottledRead::of(&error, || reason.clone()) {
                    Some(read) => CheckRecoveryStep::ReadDeferred(read),
                    None => unclassified(reason),
                },
            ));
        }
    };
    let failed_jobs: Vec<&CiCommitJobEvidence> = head_jobs
        .iter()
        .filter(|job| unsuccessful(job) && failed.iter().any(|check| answers(job, check)))
        .collect();
    // [ORB-16185] A failed required check no failed job answers is classified
    // from its own annotations, never hidden by the jobs that did fail.
    let unanswered: Vec<&(Option<String>, String)> = failed
        .iter()
        .filter(|check| {
            !recovery.handled_checks.contains(*check)
                && !head_jobs
                    .iter()
                    .any(|job| unsuccessful(job) && answers(job, check))
        })
        .collect();
    let aggregates = if unanswered.is_empty() {
        Vec::new()
    } else {
        unanswered_required(host, context, &pr.to_string(), head, &unanswered, recovery)
    };
    if failed_jobs.is_empty() && aggregates.is_empty() {
        if recovery.rerun || recovery.updated {
            // Only wait after separately failed required checks have been
            // classified. The rollup can retain the old job failure while
            // the provider queues its replacement.
            return Ok(ControlFlow::Break(CheckRecoveryStep::Wait));
        }
        return Ok(ControlFlow::Break(unclassified(
            "no failed workflow job on the pull request's head answers the failed check"
                .to_string(),
        )));
    }
    if recovery.updated
        && aggregates.is_empty()
        && failed_jobs.iter().all(|job| {
            job.job_id
                .number()
                .is_some_and(|id| recovery.handled.contains(&id))
        })
    {
        return Ok(ControlFlow::Break(CheckRecoveryStep::Wait));
    }

    let base = context.candidate.base.commit.as_str();
    let moved_tip = landing_tip.filter(|tip| *tip != base);
    let mut base_jobs = None;
    let mut tip_jobs = None;
    let mut jobs = Vec::new();
    let mut waiter_logs: Vec<(u64, String)> = Vec::new();
    let answered_checks = failed
        .iter()
        .filter(|check| failed_jobs.iter().any(|job| answers(job, check)))
        .cloned()
        .collect::<BTreeSet<_>>();
    // [model ranking §5.1] Evidence that a job's failure is the head's own,
    // by job id, and the task history it is read from (once per pass).
    let mut code: BTreeMap<u64, String> = BTreeMap::new();
    let history = || task_history(history_cell, host, &context.task_id);
    // [ORB-23813] Every failed job stopped in a declared dependency-handoff
    // step: each is transient from its failed-step name, without a log read.
    let handoff_roots = super::super::rerun::handoff_step_set(
        &host.pr_config().ci_handoff_steps,
        &head_jobs,
        failed,
    )
    .map(|(_, roots)| roots)
    .unwrap_or_default();
    for job in failed_jobs {
        let (Some(run_id), Some(job_id)) = (job.run_id.number(), job.job_id.number()) else {
            return Ok(ControlFlow::Break(unclassified(
                "a failed job reported no identity".to_string(),
            )));
        };
        let label = job_label(job);
        if let Some(conclusion) = super::super::jobs::concluded_stopped(job) {
            // The conclusion is the classification. Reading the log would
            // turn a 404 or a truncated stop into an unclassified hold.
            jobs.push(FailedJob {
                run_id: Some(run_id),
                job_id,
                label,
                class: CheckFailureClass::Transient,
                signature: super::super::jobs::conclusion_signature(conclusion),
                why: format!("the job concluded {conclusion}"),
                excerpt: String::new(),
                tip_passes: false,
            });
            continue;
        }
        if let Some(root) = handoff_roots.iter().find(|root| root.job_id == job_id) {
            jobs.push(FailedJob {
                run_id: Some(run_id),
                job_id,
                label,
                class: CheckFailureClass::Transient,
                signature: root.signature.clone(),
                why: format!(
                    "the job stopped in dependency-handoff step `{}`",
                    super::super::jobs::failed_step(job)
                ),
                excerpt: String::new(),
                tip_passes: false,
            });
            continue;
        }
        let log = match job_log(
            host,
            workspace,
            run_id,
            job_id,
            job,
            Some(recovery.deadline_ms),
        ) {
            Ok(log) => log,
            // One unreadable log is unknown evidence. Readable jobs still
            // decide when they are enough; the outcome names this job.
            Err(error) => {
                // A rate limit is the account's, not this job's: every
                // further read would be refused too, so the landing waits
                // for the reset instead.
                if let Some(read) = ThrottledRead::of(&error, || {
                    format!("the log of '{label}' could not be read: {error}")
                }) {
                    return Ok(ControlFlow::Break(CheckRecoveryStep::ReadDeferred(read)));
                }
                jobs.push(FailedJob {
                    run_id: Some(run_id),
                    job_id,
                    label,
                    class: CheckFailureClass::Unknown,
                    signature: "unreadable".to_string(),
                    why: error.to_string(),
                    excerpt: String::new(),
                    tip_passes: false,
                });
                continue;
            }
        };
        if dependency_only_log(&log) {
            waiter_logs.push((job_id, log.clone()));
        }
        let (mut class, mut why, tip_passes, signature) = match classify_logged_job(
            host,
            workspace,
            job,
            &log,
            head,
            base,
            &context.candidate.base_branch,
            moved_tip,
            &mut base_jobs,
            &mut tip_jobs,
            Some(recovery.deadline_ms),
        ) {
            Ok(classified) => classified,
            Err(deferred) => {
                return Ok(ControlFlow::Break(CheckRecoveryStep::ReadDeferred(
                    ThrottledRead {
                        reason: deferred.reason,
                        throttle: deferred.throttle,
                    },
                )));
            }
        };
        if class == CheckFailureClass::Flaky
            && super::super::suspect::has_matching_candidate_rerun(
                host,
                std::slice::from_ref(&context.task_id),
                head,
                job,
                &label,
                &signature,
                Some(pr),
            )
        {
            class = CheckFailureClass::Candidate;
            why = "the unchanged pull-request head failed the same signature again after the base check passed".into();
            code.insert(
                job_id,
                "the unchanged head failed the same check with the same signature again on its rerun"
                    .into(),
            );
        }
        if class == CheckFailureClass::Flaky
            && !(job.event.value() == Some(&CiJobEvent::PullRequest)
                && job.head_sha.text().is_none_or(|sha| sha == head)
                && {
                    let numbers = &job.association.numbers;
                    numbers.is_empty() || numbers.contains(&pr)
                })
        {
            // A push or workflow_dispatch job on the head is not the pull
            // request's run: rerunning it would not be a PR rerun, and its
            // failure alone cannot admit a repair. Hold as base-suspect
            // until a verified PR rerun repeats the signature.
            class = CheckFailureClass::BaseSuspect;
            why = "base-suspect: the failed job is not a verified pull_request run for this pull request, so it cannot authorize a rerun or repair; waiting for an unchanged-head pull_request rerun with the same failure signature".into();
        }
        if class == CheckFailureClass::BaseSuspect {
            super::super::suspect::record_base_suspect(
                host,
                std::slice::from_ref(&context.task_id),
                &json!({
                    "base":landing_tip.unwrap_or(base), "head":head, "check":label, "signature":signature,
                    "run_id":run_id, "job_id":job_id, "run_attempt":job.run_attempt, "pr_number":pr, "event":job.event,
                    "started_at":job.started_at, "completed_at":job.completed_at, "reason":why,
                }),
                Some(&context.handoff_id),
            )?;
        }
        if !matches!(
            class,
            CheckFailureClass::Transient | CheckFailureClass::Inherited
        ) && !code.contains_key(&job_id)
        {
            if matches!(
                class,
                CheckFailureClass::Candidate | CheckFailureClass::Inconclusive
            ) && super::super::suspect::cites_changed_candidate(workspace, head, base, &log)
                .unwrap_or(false)
            {
                code.insert(
                    job_id,
                    format!("the failure of '{label}' cites a file the head changed"),
                );
            } else if super::super::failure_ledger::failed_again_on_rerun(
                history(),
                head,
                job,
                &label,
            ) {
                code.insert(
                    job_id,
                    format!("'{label}' failed again on the rerun of unchanged head {head}"),
                );
            }
        }
        jobs.push(FailedJob {
            run_id: Some(run_id),
            job_id,
            label,
            class,
            signature,
            why,
            excerpt: excerpt(&log),
            tip_passes,
        });
    }
    if !waiter_logs.is_empty() {
        fold_dependency_waiters(
            &DependencyAuthScope {
                host,
                workspace,
                context,
                pr,
                head,
                base,
                deadline_ms: recovery.deadline_ms,
            },
            &waiter_logs,
            &mut jobs,
            &mut recovery.dependency_auth,
        );
    }
    let has_aggregates = !aggregates.is_empty();
    for aggregate in &aggregates {
        if aggregate.class == CheckFailureClass::Candidate {
            code.insert(
                aggregate.job_id,
                format!(
                    "the failure annotations of '{}' name only files the head changed",
                    aggregate.label
                ),
            );
        }
    }
    jobs.extend(aggregates);
    recovery
        .failures
        .record(host, context, head, pr, &jobs, &code);
    // No readable job decided. An unknown log is not itself a class.
    if jobs.is_empty()
        || jobs
            .iter()
            .all(|job| job.class == CheckFailureClass::Unknown)
    {
        return Ok(ControlFlow::Break(unclassified(unreadable_job_text(&jobs))));
    }
    Ok(ControlFlow::Continue(ClassifiedFailures {
        moved_tip,
        jobs,
        answered_checks,
        code,
        has_aggregates,
    }))
}

/// The task history the failure ledger reads, read once per pass. An
/// unreadable history reads as empty.
pub(super) fn task_history<'c, H: RuntimeHost + ?Sized>(
    history: &'c OnceCell<Vec<TaskHistoryEntry>>,
    host: &H,
    task_id: &str,
) -> &'c [TaskHistoryEntry] {
    history.get_or_init(|| {
        host.get_task_history(task_id).unwrap_or_else(|error| {
            tracing::warn!(task = %task_id, %error, "CI failure ledger: task history unreadable");
            Vec::new()
        })
    })
}
