//! Classifying failed hosted checks and recovering from them once.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use serde_json::{Value, json};

use crate::context::{HandoffLandingContext, RuntimeHost};

use super::super::super::operations;
use super::super::super::pr::{CheckState, PullRequestStatus};

use super::annotations::unanswered_required;
use super::base_update::contains;
use super::head::{classify_logged_job, onto_passing_tip};
use super::jobs::{answers, commit_jobs, describe, excerpt, job_label, unsuccessful};
use super::log::job_log;

/// What a candidate-caused failure does once the automatic repair is spent.
const REQUEUE_OUTCOME: &str = "the automatic repair is already spent, so the owner ends the claim \
     and returns the task to the backlog, where its next run repairs this candidate";

/// The class of one failed hosted check, ordered so the class that decides
/// for several jobs is the greatest: one unreadable job spends nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum CheckFailureClass {
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
    /// Untouched failures whose same-check base pass authorized a rerun.
    /// The rerun's later completion must not demand yet another base pass.
    rerun_candidates: BTreeSet<(u64, String, String)>,
    /// Repeated base-evidence observations keyed by unchanged head, check and signature.
    base_suspect_cycles: BTreeMap<(String, String, String), u64>,
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
    /// The classification each action was taken on, for the evidence.
    actions: Vec<String>,
    /// Latest required-check read during failure classification; diagnostics only.
    pub(in crate::executor::automation::vcs) required_observation: Option<Value>,
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
    /// Stop for the one automatic repair, with this evidence.
    Repair(String),
    /// [ORB-16890] That repair is spent: stop so the owner returns the task
    /// to the backlog for its next run to repair the candidate, with this
    /// evidence.
    Requeue(String),
    /// Stop in review, with this evidence.
    Stop(String),
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
pub(super) struct FailedJob {
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
        CheckRecoveryStep::Stop(format!(
            "{reason}; the failure was not classified ({why}), so the candidate stays in review"
        ))
    };
    let head_jobs = match commit_jobs(host, workspace, head) {
        Ok(jobs) => jobs,
        Err(error) => return Ok(unclassified(format!("its jobs could not be read: {error}"))),
    };
    let failed_jobs: Vec<&Value> = head_jobs
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
    if aggregates.is_empty()
        && failed_jobs.iter().all(|job| {
            job["job_id"]
                .as_u64()
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
    let answered_checks = failed
        .iter()
        .filter(|check| failed_jobs.iter().any(|job| answers(job, check)))
        .cloned()
        .collect::<BTreeSet<_>>();
    for job in failed_jobs {
        let (Some(run_id), Some(job_id)) = (job["run_id"].as_u64(), job["job_id"].as_u64()) else {
            return Ok(unclassified(
                "a failed job reported no identity".to_string(),
            ));
        };
        let label = job_label(job);
        let log = match job_log(host, workspace, run_id, job_id, job) {
            Ok(log) => log,
            Err(error) => {
                return Ok(unclassified(format!(
                    "the log of '{label}' could not be read: {error}"
                )));
            }
        };
        let (mut class, mut why, tip_passes, signature) = classify_logged_job(
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
        );
        if class == CheckFailureClass::Flaky
            && super::suspect::has_matching_candidate_rerun(
                host,
                std::slice::from_ref(&context.task_id),
                head,
                job,
                &label,
                &signature,
            )
        {
            class = CheckFailureClass::Candidate;
            why = "the unchanged pull-request head failed the same signature again after the base check passed".into();
        }
        if recovery
            .rerun_candidates
            .contains(&(run_id, label.clone(), signature.clone()))
            || (class == CheckFailureClass::Flaky && recovery.rerun)
        {
            class = CheckFailureClass::Candidate;
            why = "the untouched-path failure remains after the shared one-rerun budget was spent"
                .into();
        }
        if class == CheckFailureClass::BaseSuspect {
            super::suspect::record_base_suspect(
                host,
                std::slice::from_ref(&context.task_id),
                &json!({
                    "base":landing_tip.unwrap_or(base), "head":head, "check":label, "signature":signature,
                    "run_id":run_id, "job_id":job_id, "run_attempt":job["run_attempt"],
                    "started_at":job["started_at"], "completed_at":job["completed_at"], "reason":why,
                }),
                Some(&context.handoff_id),
            )?;
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
    jobs.extend(aggregates);
    let class = jobs
        .iter()
        .map(|job| job.class)
        .max()
        .unwrap_or(CheckFailureClass::Candidate);
    let classified = describe(class, &jobs);
    let evidence = |outcome: &str| format!("{reason}; {classified}; {outcome}");

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
                Ok(CheckRecoveryStep::Stop(evidence(&format!(
                    "escalated after {cycles} base CI observation cycles (limit {limit}): check '{check}' still fails with signature `{signature}` on unchanged head {head}; a matching pull_request rerun failure and a successful same-check agent-main run on the accepted base ancestry have not both been observed, so review the missing base/check evidence",
                ))))
            } else {
                Ok(CheckRecoveryStep::WaitForBase(evidence(
                    "waiting for a same-signature pull_request rerun and a successful same-check agent-main run on the accepted base ancestry; no candidate repair",
                )))
            }
        }
        CheckFailureClass::Transient | CheckFailureClass::Flaky if !recovery.rerun => {
            let runs: BTreeSet<u64> = jobs.iter().filter_map(|job| job.run_id).collect();
            for run_id in runs {
                host.run_private_vcs_operation(
                    operations::CI_RERUN_FAILED,
                    json!({
                        "run_id": run_id.to_string(),
                        "workspace_path": workspace.to_string_lossy(),
                    }),
                )?;
                let rerun_jobs = jobs.iter().filter(|job| job.run_id == Some(run_id)).map(|job| {
                    let original = head_jobs.iter().find(|original| {
                        original["run_id"].as_u64() == Some(run_id)
                            && original["job_id"].as_u64() == Some(job.job_id)
                    });
                    json!({
                        "job_id":job.job_id,"check":job.label,"classification":job.class.as_str(),"signature":job.signature,
                        "run_attempt":original.map(|original| original["run_attempt"].clone()).unwrap_or(Value::Null),
                        "started_at":original.map(|original| original["started_at"].clone()).unwrap_or(Value::Null),
                        "completed_at":original.map(|original| original["completed_at"].clone()).unwrap_or(Value::Null),
                    })
                }).collect::<Vec<_>>();
                host.record_handoff_landing_history(
                    &context.handoff_id,
                    "ci_check_rerun",
                    &json!({"head":head,"run_id":run_id,"jobs":rerun_jobs}).to_string(),
                )?;
            }
            recovery.rerun = true;
            recovery.rerun_candidates.extend(
                jobs.iter()
                    .filter(|job| job.class == CheckFailureClass::Flaky)
                    .filter_map(|job| {
                        Some((job.run_id?, job.label.clone(), job.signature.clone()))
                    }),
            );
            recovery.handled.extend(jobs.iter().map(|job| job.job_id));
            recovery.handled_checks.extend(answered_checks);
            recovery
                .actions
                .push(format!("reran the failed jobs: {classified}"));
            Ok(CheckRecoveryStep::Acted)
        }
        CheckFailureClass::Transient => Ok(CheckRecoveryStep::Stop(evidence(
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
        CheckFailureClass::Candidate | CheckFailureClass::Flaky
            if context.automatic_repair_spent =>
        {
            Ok(CheckRecoveryStep::Requeue(evidence(REQUEUE_OUTCOME)))
        }
        CheckFailureClass::Candidate | CheckFailureClass::Flaky => Ok(CheckRecoveryStep::Repair(
            evidence("the candidate is handed to one automatic repair"),
        )),
        CheckFailureClass::Inconclusive => Ok(CheckRecoveryStep::Stop(evidence(
            "its evidence could not be read in full, so nothing is rerun, updated or repaired \
             and the candidate stays in review",
        ))),
    }
}
