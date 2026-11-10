//! Classifying failed hosted checks and recovering from them once.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use serde_json::{Value, json};

use crate::context::{HandoffLandingContext, RuntimeHost};

use super::super::super::operations;
use super::super::super::operations::{CiCommitJobEvidence, CiJobEvent};
use super::super::super::pr::{CheckState, PullRequestStatus};

use super::annotations::unanswered_required;
use super::base_update::contains;
use super::head::{classify_logged_job, onto_passing_tip};
use super::jobs::{
    answers, commit_jobs, dependency_only_log, describe, excerpt, job_label, unsuccessful,
};
use super::log::job_log;
use super::read_wait::ThrottledRead;

/// What a candidate-caused failure does once the automatic repair is spent.
const REQUEUE_OUTCOME: &str = "the automatic repair is already spent, so the owner ends the claim \
     and returns the task to the backlog, where its next run repairs this candidate";

/// The class of one failed hosted check, ordered so the class that decides
/// for several jobs is the greatest. [`Unknown`] records a log that could
/// not be read. It never outranks a readable job, and a set of only unknown
/// logs spends nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum CheckFailureClass {
    Unknown,
    Transient,
    Inherited,
    Flaky,
    Candidate,
    BaseSuspect,
    Inconclusive,
}

impl CheckFailureClass {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Transient => "transient",
            Self::Inherited => "inherited",
            Self::Flaky => "flaky",
            Self::Candidate => "candidate",
            Self::BaseSuspect => "base-suspect",
            Self::Inconclusive => "inconclusive",
        }
    }
}

/// What this landing attempt has already done about failed checks.
#[derive(Default)]
pub(in crate::executor::automation::vcs) struct CheckRecovery {
    rerun: bool,
    pub(in crate::executor::automation::vcs) rerun_diagnostic:
        Option<super::rerun_diagnostic::CiRerunDiagnostic>,
    /// Untouched failures whose same-check base pass authorized a rerun.
    /// The rerun's later completion must not demand yet another base pass.
    /// Repeated base-evidence observations keyed by unchanged head, check and signature.
    base_suspect_cycles: BTreeMap<(String, String, String), u64>,
    /// [ORB-21586] Consecutive polls on one head whose CI retry evidence
    /// could not be read or authenticated. Bounded by `max_base_ci_cycles`,
    /// so an unreadable retry escalates within minutes, not at the deadline.
    unreadable_rerun_cycles: BTreeMap<String, u64>,
    updated: bool,
    pub(in crate::executor::automation::vcs) max_base_ci_cycles: u64,
    pub(in crate::executor::automation::vcs) waiting_for_base: Option<String>,
    pub(super) deadline_ms: u64,
    /// Failed jobs already acted on, so a rollup that still shows them
    /// before the provider registers the rerun or update waits instead.
    handled: BTreeSet<u64>,
    /// Rollup entries those jobs answered, retained while replacement jobs
    /// have not registered yet. Separately failed checks still need evidence.
    handled_checks: BTreeSet<(Option<String>, String)>,
    /// Waits this attempt took for rate-limited check reads.
    pub(in crate::executor::automation::vcs) throttled_read_waits: u64,
    /// The classification each action was taken on, for the evidence.
    actions: Vec<String>,
    /// Dependency ids authenticated for a waiter job in this attempt, or
    /// `None` when its record did not authenticate: one adapter read per
    /// waiter, however often the attempt polls.
    dependency_auth: BTreeMap<(u64, u64), Option<Vec<u64>>>,
    /// Latest required-check read during failure classification; diagnostics only.
    pub(in crate::executor::automation::vcs) required_observation: Option<Value>,
    /// [model ranking §5.1] The task's recorded failures by cause.
    pub(in crate::executor::automation::vcs) failures: super::failure_ledger::FailureLedger,
}

impl CheckRecovery {
    pub(in crate::executor::automation::vcs) fn before(
        deadline_ms: u64,
        max_base_ci_cycles: u64,
    ) -> Self {
        Self {
            deadline_ms,
            max_base_ci_cycles,
            ..Default::default()
        }
    }
    /// The actions this attempt took, for the landing's completion evidence.
    pub(in crate::executor::automation::vcs) fn evidence(&self) -> Option<Value> {
        (!self.actions.is_empty()).then(|| json!(self.actions))
    }
}

/// What the landing does next about its failed checks.
pub(in crate::executor::automation::vcs) enum CheckRecoveryStep {
    /// The failed jobs were rerun or the branch updated; read the pull
    /// request again.
    Acted,
    /// Every failed job was already acted on; keep waiting.
    Wait,
    /// A base-suspect failure waits without authorizing candidate recovery.
    WaitForBase(String),
    /// A base-suspect failure spent its observation budget with the base's
    /// own result still missing: hold in review for that result, with this
    /// evidence. Nothing is repaired or rerun, so it is a designed wait, not a
    /// failure.
    HoldForBase(String),
    /// Stop for the one automatic repair, with this evidence.
    Repair(String),
    /// [ORB-16890] That repair is spent: stop so the owner returns the task
    /// to the backlog for its next run to repair the candidate, with this
    /// evidence.
    Requeue(String),
    /// Stop in review and fail the run, with this evidence. A status that
    /// reports no head or number has no jobs to read, so it fails here too. A
    /// designed stay on a readable head — unclassified, inconclusive, or a
    /// transient failure the rerun planner will not run again — is
    /// [`Self::HoldInReview`].
    Stop(String),
    /// The candidate stays in review and the run is held, not failed.
    /// Unclassified evidence, inconclusive evidence, and a transient failure
    /// that already used its one rerun are designed stays: nothing is
    /// repaired, rerun, or updated.
    HoldInReview(String),
    /// The provider rate-limited a job or log read: wait for its reset and
    /// read again, without classifying around the missing evidence.
    ReadDeferred(super::read_wait::ThrottledRead),
}

/// The names of the checks the rollup reports failed, with their workflow.
pub(in crate::executor::automation::vcs) fn failed_checks(
    status: &PullRequestStatus,
) -> Vec<(Option<String>, String)> {
    status
        .status_checks
        .iter()
        .flatten()
        .filter(|check| {
            check.state == Some(CheckState::Failed) || check.owed_state == Some(CheckState::Failed)
        })
        .filter_map(|check| Some((check.workflow_name.clone(), check.name.clone()?)))
        .collect()
}

/// One failed job, or one failed required check run outside a workflow, on
/// the pull request's head.
pub(in crate::executor::automation::vcs) struct FailedJob {
    /// The workflow run to rerun; none for a check run outside a workflow.
    pub(super) run_id: Option<u64>,
    /// The job's, or the check run's, provider identity.
    pub(super) job_id: u64,
    pub(super) label: String,
    pub(super) class: CheckFailureClass,
    pub(super) signature: String,
    pub(super) why: String,
    pub(super) excerpt: String,
    /// The moved base tip's same job succeeded. A failure that remains on a
    /// head containing that tip is then the candidate's, not the base's.
    pub(super) tip_passes: bool,
}

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
    if let Some(reason) = super::rerun::infrastructure_backoff(
        host,
        std::slice::from_ref(&context.task_id),
        head,
        &pr.to_string(),
    )? {
        return Ok(CheckRecoveryStep::WaitForBase(reason));
    }
    let head_jobs = match commit_jobs(host, workspace, head) {
        Ok(jobs) => jobs,
        Err(error) => {
            let reason = format!("its jobs could not be read: {error}");
            return Ok(match ThrottledRead::of(&error, || reason.clone()) {
                Some(read) => CheckRecoveryStep::ReadDeferred(read),
                None => unclassified(reason),
            });
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
            return Ok(CheckRecoveryStep::Wait);
        }
        return Ok(unclassified(
            "no failed workflow job on the pull request's head answers the failed check"
                .to_string(),
        ));
    }
    if recovery.updated
        && aggregates.is_empty()
        && failed_jobs.iter().all(|job| {
            job.job_id
                .number()
                .is_some_and(|id| recovery.handled.contains(&id))
        })
    {
        return Ok(CheckRecoveryStep::Wait);
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
    let history = std::cell::OnceCell::new();
    let history = || {
        history.get_or_init(|| {
            host.get_task_history(&context.task_id)
                .unwrap_or_else(|error| {
                    tracing::warn!(task = %context.task_id, %error, "CI failure ledger: task history unreadable");
                    Vec::new()
                })
        })
    };
    // [ORB-23813] Every failed job stopped in a declared dependency-handoff
    // step: each is transient from its failed-step name, without a log read.
    let handoff_roots =
        super::rerun::handoff_step_set(&host.pr_config().ci_handoff_steps, &head_jobs, failed)
            .map(|(_, roots)| roots)
            .unwrap_or_default();
    for job in failed_jobs {
        let (Some(run_id), Some(job_id)) = (job.run_id.number(), job.job_id.number()) else {
            return Ok(unclassified(
                "a failed job reported no identity".to_string(),
            ));
        };
        let label = job_label(job);
        if let Some(conclusion) = super::jobs::concluded_stopped(job) {
            // The conclusion is the classification. Reading the log would
            // turn a 404 or a truncated stop into an unclassified hold.
            jobs.push(FailedJob {
                run_id: Some(run_id),
                job_id,
                label,
                class: CheckFailureClass::Transient,
                signature: super::jobs::conclusion_signature(conclusion),
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
                    super::jobs::failed_step(job)
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
                    return Ok(CheckRecoveryStep::ReadDeferred(read));
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
                return Ok(CheckRecoveryStep::ReadDeferred(ThrottledRead {
                    reason: deferred.reason,
                    throttle: deferred.throttle,
                }));
            }
        };
        if class == CheckFailureClass::Flaky
            && super::suspect::has_matching_candidate_rerun(
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
            super::suspect::record_base_suspect(
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
            ) && super::suspect::cites_changed_candidate(workspace, head, base, &log)
                .unwrap_or(false)
            {
                code.insert(
                    job_id,
                    format!("the failure of '{label}' cites a file the head changed"),
                );
            } else if super::failure_ledger::failed_again_on_rerun(history(), head, job, &label) {
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
            host,
            workspace,
            context,
            pr,
            head,
            base,
            &waiter_logs,
            &mut jobs,
            recovery.deadline_ms,
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
        return Ok(unclassified(unreadable_job_text(&jobs)));
    }
    let planned = if has_aggregates {
        Ok(super::rerun::CiRerunStep::NotEligible)
    } else {
        super::rerun::plan_ci_rerun(
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
            super::rerun::CiRerunStep::Wait(reason)
        } else {
            super::rerun::CiRerunStep::Unreadable(reason)
        }
    });
    let mut unreadable_retry = None;
    if let super::rerun::CiRerunStep::Unreadable(reason) = &planned {
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
        super::rerun::CiRerunStep::Backoff(reason) | super::rerun::CiRerunStep::Wait(reason) => {
            return Ok(CheckRecoveryStep::WaitForBase(reason));
        }
        super::rerun::CiRerunStep::Unreadable(reason) if unreadable_retry.is_none() => {
            return Ok(CheckRecoveryStep::WaitForBase(reason));
        }
        super::rerun::CiRerunStep::Unreadable(_) => {}
        super::rerun::CiRerunStep::Acted(plan) => {
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
        super::rerun::CiRerunStep::BudgetSpent(reason) => {
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
        super::rerun::CiRerunStep::NotEligible => {}
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
        && !super::failure_ledger::head_was_rerun(history(), head)
    {
        match super::rerun::request_classification_rerun(
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
            // failure does, and a spent repair stops in review.
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
                    super::super::super::completion_deadline::Deadline(previous.deadline_ms)
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
        CheckFailureClass::Candidate => Ok(CheckRecoveryStep::Repair(evidence(
            "the candidate is handed to one automatic repair",
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

/// Dependency job ids a waiter's record names, parsed without trusting
/// them: only a gate for spending the authenticated read, never evidence.
fn dependency_ids_in_log(log: &str) -> Vec<u64> {
    let Some((_, body)) = log.split_once("orbit-dependency-evidence:") else {
        return Vec::new();
    };
    let Some(record) = body
        .lines()
        .next()
        .and_then(|line| serde_json::from_str::<Value>(line.trim()).ok())
    else {
        return Vec::new();
    };
    record["dependencies"]
        .as_array()
        .map(|deps| {
            deps.iter()
                .filter_map(|dep| dep["job_id"].as_u64())
                .collect()
        })
        .unwrap_or_default()
}

/// Fold authenticated dependency waiters into the jobs they name: a waiter
/// whose only diagnostic is an authenticated dependency record takes no
/// class of its own, so the aggregate class comes from its dependencies.
/// An unauthenticated or malformed record keeps its classified class.
#[allow(clippy::too_many_arguments)]
fn fold_dependency_waiters<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &std::path::Path,
    context: &HandoffLandingContext,
    pr: u64,
    head: &str,
    base: &str,
    waiter_logs: &[(u64, String)],
    jobs: &mut [FailedJob],
    deadline_ms: u64,
    dependency_auth: &mut BTreeMap<(u64, u64), Option<Vec<u64>>>,
) {
    let waiter_ids: BTreeSet<u64> = waiter_logs.iter().map(|(id, _)| *id).collect();
    let mut folded: BTreeSet<u64> = BTreeSet::new();
    for _ in 0..waiter_logs.len() {
        let mut progressed = false;
        for (waiter_id, waiter_log) in waiter_logs {
            if folded.contains(waiter_id) {
                continue;
            }
            // Authentication re-reads provider logs through the adapter, so
            // only spend it when folding can change the deciding class: a
            // dependency the waiter masks is transient or candidate. A
            // waiter beside an inherited or otherwise waiting dependency
            // keeps today's route and spends no extra read.
            let named: Vec<u64> = dependency_ids_in_log(waiter_log);
            let masks_deciding = jobs.iter().any(|job| {
                named.contains(&job.job_id)
                    && matches!(
                        job.class,
                        CheckFailureClass::Transient | CheckFailureClass::Candidate
                    )
            });
            if !masks_deciding {
                continue;
            }
            let Some(run_id) = jobs
                .iter()
                .find(|job| job.job_id == *waiter_id)
                .and_then(|job| job.run_id)
            else {
                continue;
            };
            let dependency_ids = match dependency_auth.get(&(run_id, *waiter_id)) {
                Some(cached) => cached.clone(),
                None => {
                    let authenticated = authenticated_dependency_ids(
                        host,
                        workspace,
                        context,
                        pr,
                        head,
                        base,
                        run_id,
                        *waiter_id,
                        jobs,
                        deadline_ms,
                    );
                    dependency_auth.insert((run_id, *waiter_id), authenticated.clone());
                    authenticated
                }
            };
            let Some(dependency_ids) = dependency_ids else {
                continue;
            };
            // A chained waiter folds only after its own dependencies have.
            if dependency_ids.iter().any(|id| {
                !jobs.iter().any(|job| job.job_id == *id)
                    || (waiter_ids.contains(id) && !folded.contains(id))
            }) {
                continue;
            }
            let dependencies: Vec<&FailedJob> = jobs
                .iter()
                .filter(|job| dependency_ids.contains(&job.job_id))
                .collect();
            if dependencies.is_empty()
                || dependencies
                    .iter()
                    .any(|job| job.class == CheckFailureClass::Unknown)
            {
                continue;
            }
            let class = dependencies
                .iter()
                .map(|job| job.class)
                .max()
                .unwrap_or(CheckFailureClass::Candidate);
            let labels = dependencies
                .iter()
                .map(|job| format!("'{}'", job.label))
                .collect::<Vec<_>>()
                .join(", ");
            if let Some(waiter) = jobs.iter_mut().find(|job| job.job_id == *waiter_id) {
                waiter.class = class;
                waiter.why = format!(
                    "folded into {labels}: its only diagnostic is an authenticated \
                     dependency record naming that job, so it takes its dependencies' class"
                );
            }
            folded.insert(*waiter_id);
            progressed = true;
        }
        if !progressed {
            break;
        }
    }
}

/// Authenticate one waiter's dependency record through the same adapter the
/// rerun planner uses, returning the dependency job ids it names. Any
/// failure — malformed, forged or unproven closure — returns `None`.
#[allow(clippy::too_many_arguments)]
fn authenticated_dependency_ids<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &std::path::Path,
    context: &HandoffLandingContext,
    pr: u64,
    head: &str,
    base: &str,
    run_id: u64,
    waiter_id: u64,
    jobs: &[FailedJob],
    deadline_ms: u64,
) -> Option<Vec<u64>> {
    let roots: Vec<orbit_types::workflow::CiRerunRoot> = jobs
        .iter()
        .filter(|job| {
            job.run_id == Some(run_id)
                && job.job_id != waiter_id
                && job.class != CheckFailureClass::Unknown
                && !job.signature.is_empty()
        })
        .map(|job| {
            // The planner authenticates a stopped job only as a transient root
            // carrying its conclusion signature; a log-classified job stays flaky.
            let classification = if super::jobs::is_conclusion_signature(&job.signature) {
                CheckFailureClass::Transient.as_str()
            } else {
                "flaky"
            };
            orbit_types::workflow::CiRerunRoot {
                job_id: job.job_id,
                check: job.label.clone(),
                classification: classification.to_string(),
                signature: job.signature.clone(),
            }
        })
        .collect();
    if roots.is_empty() {
        return None;
    }
    let answer = host
        .run_private_vcs_operation(
            operations::CI_RERUN_PLAN,
            json!({
                "workspace_path": workspace,
                "pr": pr.to_string(),
                "head_sha": head,
                "head_branch": context.candidate.source_branch,
                "base_branch": context.candidate.base_branch,
                "accepted_base": base,
                "run_id": run_id,
                "roots": roots,
                "deadline_ms": deadline_ms,
            }),
        )
        .ok()?;
    let dependencies = answer["plan"]["authentication"]["dependencies"].as_array()?;
    let record = dependencies
        .iter()
        .find(|record| record["job_id"].as_u64() == Some(waiter_id))?;
    let ids: Vec<u64> = record["dependency_ids"]
        .as_array()?
        .iter()
        .filter_map(Value::as_u64)
        .collect();
    (!ids.is_empty()).then_some(ids)
}

/// The unclassified reason for logs that could not be read, one job at a time.
pub(super) fn unreadable_job_text(jobs: &[FailedJob]) -> String {
    let text = jobs
        .iter()
        .filter(|job| job.class == CheckFailureClass::Unknown)
        .map(|job| format!("the log of '{}' could not be read: {}", job.label, job.why))
        .collect::<Vec<_>>()
        .join("; ");
    if text.is_empty() {
        "no readable failed-job log decided the classification".to_string()
    } else {
        text
    }
}
