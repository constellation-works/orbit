//! [ORB-15956] What the owner's landing does when a pull request's hosted
//! check fails on its pinned head.
//!
//! The landing already waits on the pull request's checks. A failed one used
//! to stop the candidate in review until an operator rescued it, even when
//! the failure was not the candidate's. Each failed job is now classified
//! from its failed-step log, with the CI sweep's own log read and error
//! signature:
//!
//! - **inherited**: the same signature fails on the candidate's base commit,
//!   or the base branch has moved past that base and the same job passes on
//!   its tip. The landing merges the current base into the pull request's
//!   branch once, through the provider's branch update, and waits for the
//!   checks again on the new head. No agent runs. A failure that is still
//!   there after that one update, on a base tip where the same job passes,
//!   is the candidate's: the landing stops for the one automatic repair,
//!   with the failed job, its signature and a log excerpt, unless that
//!   repair is already spent, in which case it stops in review. An unmoved
//!   base that is still red stops in review without a repair.
//! - **transient**: the log shows the network or the runner failing, by the
//!   rule the validation gates already rerun on. The landing reruns the
//!   failed jobs once and waits again.
//! - **candidate**: the failed step names a diagnostic, and the base's own
//!   jobs were read and do not explain it. The landing stops for the same
//!   single automatic repair a conflicting base gets, with the failed step's
//!   signature and log excerpt in the stop evidence the repair leaf reads.
//! - **inconclusive**: the evidence for any of the above could not be read:
//!   a job query or log read failed, a log read recovered no text, or a log
//!   names no diagnostic, so its signature is only the failed step's name.
//!   A failed read is not a successful empty answer, and neither proves the
//!   base or the candidate at fault, so the landing stops in review and
//!   spends nothing.
//!
//! [ORB-16185] A required check that no failed job answers, such as a
//! provider app's aggregate result beside passing analysis jobs, is not
//! hidden by the jobs that did fail: every failed required check is
//! classified. Its evidence is its own failure annotations
//! ([`annotations`]); they make it a **candidate** failure only when each
//! annotated source is a file the candidate changed from its base. Anything
//! else, including annotations on files the candidate did not change, is
//! **inconclusive**. A failed check branch protection does not require keeps
//! its previous handling.
//!
//! The rerun, the branch update and the repair are each spent once. A
//! failure past that budget, or one this module cannot classify, stops the
//! landing in review, with the classification in the stop evidence. The
//! repair an inherited failure earns after its one update is that same
//! single repair, not a second update or a second repair.
//!
//! A branch update moves the pull request's head off the accepted candidate.
//! [`verified_base_update`] accepts the new head only when it is a merge of
//! that candidate with a commit already on the landing branch whose tree is
//! exactly the clean merge of the two, so nothing but the base's own advance
//! reaches the merge. The provider merges the base as it is when the update
//! runs, so the caller refreshes the landing ref before asking. The owner's
//! accepted candidate, its validation evidence and the observation the store
//! rechecks stay the validated commit.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::OrbitError;
use serde_json::{Value, json};

use crate::context::{HandoffLandingContext, RuntimeHost};

use super::super::ci::log_signature::{
    LineKind, classify_log_lines, diagnostic_excerpt, error_signature,
};
use super::baseline::network_inconclusive;
use super::git::{git_command_success, git_output};
use super::operations;
use super::pr::{CheckState, PullRequestStatus, RequiredChecks};

mod annotations;
mod log;
use annotations::{source_annotations, unanswered_required};
use log::job_log;

/// Largest log excerpt a stop's evidence carries per failed job.
const MAX_EXCERPT_BYTES: usize = 2 * 1024;

/// Lines a runner prints when the machine, not the code, gave out.
const RUNNER_LOSS: &[&str] = &[
    "the runner has received a shutdown signal",
    "lost communication with the server",
    "the operation was canceled",
    "no space left on device",
    "the hosted runner encountered an error",
];

/// The class of one failed hosted check, ordered so the class that decides
/// for several jobs is the greatest: one unreadable job spends nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CheckFailureClass {
    Transient,
    Inherited,
    Candidate,
    Inconclusive,
}

impl CheckFailureClass {
    fn as_str(self) -> &'static str {
        match self {
            Self::Transient => "transient",
            Self::Inherited => "inherited",
            Self::Candidate => "candidate",
            Self::Inconclusive => "inconclusive",
        }
    }
}

/// What this landing attempt has already done about failed checks.
#[derive(Default)]
pub(super) struct CheckRecovery {
    rerun: bool,
    updated: bool,
    deadline_ms: u64,
    /// Failed jobs already acted on, so a rollup that still shows them
    /// before the provider registers the rerun or update waits instead.
    handled: BTreeSet<u64>,
    /// The classification each action was taken on, for the evidence.
    actions: Vec<String>,
}

impl CheckRecovery {
    pub(super) fn before(deadline_ms: u64) -> Self {
        Self {
            deadline_ms,
            ..Default::default()
        }
    }
    /// The actions this attempt took, for the landing's completion evidence.
    pub(super) fn evidence(&self) -> Option<Value> {
        (!self.actions.is_empty()).then(|| json!(self.actions))
    }
}

/// What the landing does next about its failed checks.
pub(super) enum CheckRecoveryStep {
    /// The failed jobs were rerun or the branch updated; read the pull
    /// request again.
    Acted,
    /// Every failed job was already acted on; keep waiting.
    Wait,
    /// Stop for the one automatic repair, with this evidence.
    Repair(String),
    /// Stop in review, with this evidence.
    Stop(String),
}

/// The names of the checks the rollup reports failed, with their workflow.
pub(super) fn failed_checks(status: &PullRequestStatus) -> Vec<(Option<String>, String)> {
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
struct FailedJob {
    /// The workflow run to rerun; none for a check run outside a workflow.
    run_id: Option<u64>,
    /// The job's, or the check run's, provider identity.
    job_id: u64,
    label: String,
    class: CheckFailureClass,
    signature: String,
    why: String,
    excerpt: String,
    /// The moved base tip's same job succeeded. A failure that remains after
    /// the one branch update is then the candidate's, not the base's.
    tip_passes: bool,
}

/// Classify the failed checks on `status`'s head and act on them.
///
/// `reason` is the failure as the landing words it. `landing_tip` is the
/// landing branch's current commit in the owner checkout.
pub(super) fn recover_failed_checks<H: RuntimeHost + ?Sized>(
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
    if failed_jobs.is_empty() && (recovery.rerun || recovery.updated) {
        // A rerun or update the provider has not registered yet still shows
        // the old failure in the rollup while its jobs are queued.
        return Ok(CheckRecoveryStep::Wait);
    }
    // [ORB-16185] A failed required check no failed job answers is classified
    // from its own annotations, never hidden by the jobs that did fail.
    let unanswered: Vec<&(Option<String>, String)> = failed
        .iter()
        .filter(|check| {
            !head_jobs
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
        let signature = error_signature(&log, failed_step(job));
        let (class, why, tip_passes) = if let Some(evidence) = transient_evidence(&log) {
            (
                CheckFailureClass::Transient,
                format!("the log shows `{evidence}`"),
                false,
            )
        } else if signature.step_fallback {
            (
                CheckFailureClass::Inconclusive,
                "its log names no diagnostic, only the failed step".to_string(),
                false,
            )
        } else {
            match inherited(
                host,
                workspace,
                job,
                &signature.text,
                base,
                moved_tip,
                &mut base_jobs,
                &mut tip_jobs,
            ) {
                Err(gap) => (CheckFailureClass::Inconclusive, gap, false),
                Ok(Some(found)) => (CheckFailureClass::Inherited, found.why, found.tip_passes),
                Ok(None) => (
                    CheckFailureClass::Candidate,
                    format!(
                        "base {base} does not fail it that way{}",
                        moved_tip.map_or_else(String::new, |tip| format!(
                            " and it does not pass on the current base {tip}"
                        ))
                    ),
                    false,
                ),
            }
        };
        jobs.push(FailedJob {
            run_id: Some(run_id),
            job_id,
            label,
            class,
            signature: signature.text,
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
        CheckFailureClass::Transient if !recovery.rerun => {
            let runs: BTreeSet<u64> = jobs.iter().filter_map(|job| job.run_id).collect();
            for run_id in runs {
                host.run_private_vcs_operation(
                    operations::CI_RERUN_FAILED,
                    json!({
                        "run_id": run_id.to_string(),
                        "workspace_path": workspace.to_string_lossy(),
                    }),
                )?;
            }
            recovery.rerun = true;
            recovery.handled.extend(jobs.iter().map(|job| job.job_id));
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
                && onto_passing_tip(&jobs) =>
        {
            // The one update already landed the candidate on a base tip where
            // the same job passes, so the failure that remains is the
            // candidate's. It takes the same single repair a candidate-class
            // failure does, and a spent repair stops in review.
            if context.automatic_repair_spent {
                Ok(CheckRecoveryStep::Stop(evidence(
                    "this candidate already is the one automatic repair, so it stays in review",
                )))
            } else {
                Ok(CheckRecoveryStep::Repair(evidence(
                    "it failed again after one branch update onto a base where the same job \
                     passes, so the candidate is handed to one automatic repair",
                )))
            }
        }
        CheckFailureClass::Inherited
            if recovery.updated || head != context.candidate.candidate.commit =>
        {
            Ok(CheckRecoveryStep::Stop(evidence(
                "it failed again after one branch update, so the candidate stays in review",
            )))
        }
        CheckFailureClass::Inherited => {
            let Some(tip) = moved_tip.filter(|tip| !contains(workspace, tip, head)) else {
                return Ok(CheckRecoveryStep::Stop(evidence(
                    "the base branch has not moved past it, so the base is still red and the \
                     candidate stays in review",
                )));
            };
            let request = orbit_types::workflow::PrBaseUpdateReservation {
                execution: context.handoff_id.clone(),
                repository: context.candidate.repository.clone(),
                pr_number: pr.to_string(),
                head_sha: head.into(),
                head_branch: context.candidate.source_branch.clone(),
                base_branch: context.candidate.base_branch.clone(),
                base_sha: tip.into(),
                deadline_ms: recovery.deadline_ms,
                evidence: json!({"cause":"inherited","classification":classified}),
            };
            match host.admit_handoff_base_update(&context.handoff_id, &request)? {
                orbit_types::workflow::PrBaseUpdateAdmission::Reserved => {}
                orbit_types::workflow::PrBaseUpdateAdmission::Observe(previous) => {
                    super::completion_deadline::Deadline(previous.deadline_ms).remaining()?;
                    return Ok(CheckRecoveryStep::Wait);
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
            recovery.actions.push(format!(
                "merged base tip {tip} into the pull request's branch: {classified}"
            ));
            Ok(CheckRecoveryStep::Acted)
        }
        CheckFailureClass::Candidate if context.automatic_repair_spent => {
            Ok(CheckRecoveryStep::Stop(evidence(
                "this candidate already is the one automatic repair, so it stays in review",
            )))
        }
        CheckFailureClass::Candidate => Ok(CheckRecoveryStep::Repair(evidence(
            "the candidate is handed to one automatic repair",
        ))),
        CheckFailureClass::Inconclusive => Ok(CheckRecoveryStep::Stop(evidence(
            "its evidence could not be read in full, so nothing is rerun, updated or repaired \
             and the candidate stays in review",
        ))),
    }
}

/// What the failed checks on a pull request's head show an `--auto-fix`
/// watcher, read with the same job and log answers the landing uses.
pub(super) enum HeadCheckEvidence {
    /// Every failed check is a workflow job whose log was read and shows no
    /// runner or network failure, or a required check run outside a workflow
    /// whose authenticated annotations name source failures: hand it to a
    /// repair with this evidence and the retained provider answers.
    Repair {
        evidence: String,
        annotations: Vec<Value>,
    },
    /// Not a failure a code repair can clear, or not one this read could
    /// establish, for this reason.
    Stop(String),
}

/// Read the failed jobs behind `failed` on `head`, the exact commit the
/// rollup reported, so the evidence a repair receives is bound to it.
///
/// A required check run outside a workflow is attributed to code only by
/// its own source failure annotations ([`annotations`]); any other check no
/// workflow job answers (an external status, a policy gate) is not. An
/// unreadable job or log proves nothing; a log that shows the runner or the
/// network failing is not the candidate's. Each stops without a repair.
pub(super) fn head_check_evidence<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    pr: &str,
    head: &str,
    failed: &[(Option<String>, String)],
    required: &RequiredChecks,
) -> HeadCheckEvidence {
    let jobs = match commit_jobs(host, workspace, head) {
        Ok(jobs) => jobs,
        Err(error) => {
            return HeadCheckEvidence::Stop(format!(
                "the jobs on head {head} could not be read: {error}"
            ));
        }
    };
    let mut details = Vec::new();
    let mut annotations = Vec::new();
    for check in failed {
        let label = match &check.0 {
            Some(workflow) => format!("{workflow} / {}", check.1),
            None => check.1.clone(),
        };
        let Some(job) = jobs
            .iter()
            .find(|job| unsuccessful(job) && answers(job, check))
        else {
            match source_annotations(host, workspace, pr, head, check, required, None) {
                Ok(found) => {
                    details.push(found.detail);
                    annotations.push(found.provider);
                    continue;
                }
                Err(gap) => {
                    return HeadCheckEvidence::Stop(format!(
                        "no failed workflow job on head {head} answers check '{label}', and its \
                         annotations do not establish a source failure ({gap}), so it is not a \
                         failure a code repair is known to clear"
                    ));
                }
            }
        };
        let (Some(run_id), Some(job_id)) = (job["run_id"].as_u64(), job["job_id"].as_u64()) else {
            return HeadCheckEvidence::Stop(format!(
                "the failed job behind check '{label}' reported no identity"
            ));
        };
        let log = match job_log(host, workspace, run_id, job_id, job) {
            Ok(log) => log,
            Err(error) => {
                return HeadCheckEvidence::Stop(format!(
                    "the log of '{}' could not be read: {error}",
                    job_label(job)
                ));
            }
        };
        if let Some(evidence) = transient_evidence(&log) {
            return HeadCheckEvidence::Stop(format!(
                "the log of '{}' shows `{evidence}`: the runner or the network failed, not \
                 the candidate",
                job_label(job)
            ));
        }
        let signature = error_signature(&log, failed_step(job));
        if signature.step_fallback {
            return HeadCheckEvidence::Stop(format!(
                "the log of '{}' names no emitted diagnostic, only the failed step",
                job_label(job)
            ));
        }
        details.push(format!(
            "'{}' failed in step `{}` on head {head} (run {run_id}, job {job_id}) with signature `{}`; log excerpt:\n{}",
            job_label(job),
            failed_step(job),
            signature.text,
            excerpt(&log)
        ));
    }
    if details.is_empty() {
        return HeadCheckEvidence::Stop(format!("the rollup names no failed check on head {head}"));
    }
    HeadCheckEvidence::Repair {
        evidence: details.join("\n"),
        annotations,
    }
}

/// The jobs on one commit, or why they could not be read; read once per
/// landing attempt.
type CommitJobs = Option<Result<Vec<Value>, String>>;

/// An inherited failure, and whether the moved base tip passes the same job.
struct Inheritance {
    why: String,
    /// True when the moved tip's same job succeeded. A failed tip read does
    /// not set this and does not turn a base-signature match inconclusive:
    /// the first update still happens, and the failure is not treated as
    /// surviving onto a passing tip.
    tip_passes: bool,
}

/// Whether the failure of `job`, with genuine `signature`, is the base's:
/// the same signature fails on the base commit, or the base branch moved and
/// the same job passes there. `Ok(None)` is an answer read in full; `Err`
/// names the evidence that could not be read.
#[allow(clippy::too_many_arguments)]
fn inherited<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    job: &Value,
    signature: &str,
    base: &str,
    moved_tip: Option<&str>,
    base_jobs: &mut CommitJobs,
    tip_jobs: &mut CommitJobs,
) -> Result<Option<Inheritance>, String> {
    let base_jobs = base_jobs
        .get_or_insert_with(|| commit_jobs(host, workspace, base).map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|error| format!("the jobs on base {base} could not be read: {error}"))?;
    for candidate in base_jobs
        .iter()
        .filter(|candidate| same_job(candidate, job) && unsuccessful(candidate))
    {
        let (Some(run_id), Some(job_id)) =
            (candidate["run_id"].as_u64(), candidate["job_id"].as_u64())
        else {
            return Err(format!("a failed job on base {base} reported no identity"));
        };
        let log = job_log(host, workspace, run_id, job_id, candidate).map_err(|error| {
            format!("the log of the failed job on base {base} could not be read: {error}")
        })?;
        let found = error_signature(&log, failed_step(candidate));
        if found.step_fallback {
            return Err(format!(
                "the failed job on base {base} logs no diagnostic, only the failed step"
            ));
        }
        if found.text == signature {
            let tip_passes =
                moved_tip.is_some_and(|tip| same_job_passes(host, workspace, job, tip, tip_jobs));
            return Ok(Some(Inheritance {
                why: format!("the same signature fails on base {base}"),
                tip_passes,
            }));
        }
    }
    let Some(tip) = moved_tip else {
        return Ok(None);
    };
    let tip_jobs = tip_jobs
        .get_or_insert_with(|| commit_jobs(host, workspace, tip).map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|error| {
            format!("the jobs on the current base {tip} could not be read: {error}")
        })?;
    Ok(tip_jobs
        .iter()
        .any(|candidate| {
            same_job(candidate, job) && candidate["conclusion"].as_str() == Some("success")
        })
        .then(|| Inheritance {
            why: format!("the base branch moved from {base} to {tip}, where it passes"),
            tip_passes: true,
        }))
}

/// Whether every inherited job passes on the moved base tip. One that does
/// not keeps the post-update review stop: the base may still explain it.
fn onto_passing_tip(jobs: &[FailedJob]) -> bool {
    jobs.iter()
        .any(|job| job.class == CheckFailureClass::Inherited)
        && jobs
            .iter()
            .filter(|job| job.class == CheckFailureClass::Inherited)
            .all(|job| job.tip_passes)
}

/// Whether `tip` reports `job` succeeded. A failed read is not a pass, and
/// it is cached so a later job sees the same answer.
fn same_job_passes<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    job: &Value,
    tip: &str,
    tip_jobs: &mut CommitJobs,
) -> bool {
    let read = tip_jobs.get_or_insert_with(|| {
        commit_jobs(host, workspace, tip).map_err(|error| error.to_string())
    });
    read.as_ref().is_ok_and(|jobs| {
        jobs.iter().any(|candidate| {
            same_job(candidate, job) && candidate["conclusion"].as_str() == Some("success")
        })
    })
}

/// A shared-base failure already repaired on a newer base. Every failed
/// required job must have the same diagnostic on the old base and succeed on
/// the new one. Missing evidence never authorizes a refresh.
pub(super) fn landed_check_repair<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    head: &str,
    old_base: &str,
    new_base: &str,
    failed: &[(Option<String>, String)],
) -> Option<Value> {
    if old_base == new_base {
        return None;
    }
    let jobs = commit_jobs(host, workspace, head).ok()?;
    let base_jobs = commit_jobs(host, workspace, old_base).ok()?;
    let tip_jobs = commit_jobs(host, workspace, new_base).ok()?;
    let mut evidence = Vec::new();
    for check in failed {
        let job = jobs
            .iter()
            .find(|job| unsuccessful(job) && answers(job, check))?;
        let log = job_log(
            host,
            workspace,
            job["run_id"].as_u64()?,
            job["job_id"].as_u64()?,
            job,
        )
        .ok()?;
        if transient_evidence(&log).is_some() {
            return None;
        }
        let signature = error_signature(&log, failed_step(job));
        if signature.step_fallback {
            return None;
        }
        let mut matched = None;
        for base_job in base_jobs
            .iter()
            .filter(|base_job| same_job(base_job, job) && unsuccessful(base_job))
        {
            let base_log = job_log(
                host,
                workspace,
                base_job["run_id"].as_u64()?,
                base_job["job_id"].as_u64()?,
                base_job,
            )
            .ok()?;
            let base_signature = error_signature(&base_log, failed_step(base_job));
            if !base_signature.step_fallback && base_signature.text == signature.text {
                matched = Some(base_job);
                break;
            }
        }
        let base_job = matched?;
        let passing = tip_jobs
            .iter()
            .find(|tip| same_job(tip, job) && tip["conclusion"].as_str() == Some("success"))?;
        evidence.push(json!({"failed_job":job,"base_job":base_job,"passing_job":passing,"signature":signature.text,"log_excerpt":excerpt(&log)}));
    }
    (!evidence.is_empty()).then(
        || json!({"failed_head":head,"old_base":old_base,"landed_base":new_base,"checks":evidence}),
    )
}

/// The step a failed job's signature falls back to naming.
fn failed_step(job: &Value) -> &str {
    job["failed_step"]
        .as_str()
        .or_else(|| job["name"].as_str())
        .unwrap_or_default()
}

/// Every job on `commit`. An answer without a job list is a failed read,
/// never an empty one.
fn commit_jobs<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    commit: &str,
) -> Result<Vec<Value>, OrbitError> {
    let response = host.run_private_vcs_operation(
        operations::CI_COMMIT_JOBS,
        json!({"commit": commit, "workspace_path": workspace.to_string_lossy()}),
    )?;
    response["jobs"].as_array().cloned().ok_or_else(|| {
        OrbitError::Execution(format!("the jobs answer for {commit} carried no job list"))
    })
}

/// A job that finished without passing.
fn unsuccessful(job: &Value) -> bool {
    super::super::ci::unsuccessful_conclusion(job["conclusion"].as_str())
}

/// Whether rollup entry `check` is this job's check run.
fn answers(job: &Value, (workflow, name): &(Option<String>, String)) -> bool {
    job["name"].as_str().map(str::trim) == Some(name.as_str())
        && workflow
            .as_deref()
            .is_none_or(|workflow| job["workflow"].as_str().map(str::trim) == Some(workflow))
}

fn same_job(left: &Value, right: &Value) -> bool {
    left["name"] == right["name"] && left["workflow"] == right["workflow"]
}

fn job_label(job: &Value) -> String {
    let name = job["name"].as_str().unwrap_or("unnamed job");
    match job["workflow"].as_str() {
        Some(workflow) => format!("{workflow} / {name}"),
        None => name.to_string(),
    }
}

/// The network or runner failure a log shows, if any.
fn transient_evidence(log: &str) -> Option<String> {
    let emitted = classify_log_lines(log)
        .into_iter()
        .filter(|(kind, _)| {
            !matches!(
                kind,
                LineKind::RunCommand
                    | LineKind::CommandBody
                    | LineKind::ParamDump
                    | LineKind::EndGroup
            )
        })
        .map(|(_, line)| line)
        .collect::<Vec<_>>()
        .join("\n");
    network_inconclusive(&emitted).or_else(|| {
        let lowered = emitted.to_ascii_lowercase();
        RUNNER_LOSS
            .iter()
            .find(|marker| lowered.contains(*marker))
            .map(|marker| (*marker).to_string())
    })
}

/// Keep the selected emitted diagnostic even when cleanup dominates the tail.
fn excerpt(log: &str) -> String {
    diagnostic_excerpt(log, MAX_EXCERPT_BYTES)
}

fn describe(class: CheckFailureClass, jobs: &[FailedJob]) -> String {
    let details = jobs
        .iter()
        .map(|job| {
            format!(
                "'{}' ({}: {}) failed with signature `{}`; log excerpt:\n{}",
                job.label,
                job.class.as_str(),
                job.why,
                job.signature,
                job.excerpt
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("classified `{}`: {details}", class.as_str())
}

fn contains(workspace: &Path, ancestor: &str, descendant: &str) -> bool {
    git_command_success(
        workspace,
        &["merge-base", "--is-ancestor", ancestor, descendant],
    )
    .unwrap_or(false)
}

/// Whether `head` is a provider branch update of the validated `candidate`:
/// a two-parent merge of the candidate with a commit the landing ref already
/// contains, whose tree is exactly the clean merge of the two.
///
/// Fetches the head branch first when the object is not in the owner
/// checkout. Any answer this cannot read is a refusal.
pub(super) fn verified_base_update(
    workspace: &Path,
    candidate: &str,
    head: &str,
    head_branch: &str,
    landing_ref: &str,
) -> Result<(), String> {
    let object = format!("{head}^{{commit}}");
    if !git_command_success(workspace, &["cat-file", "-e", &object]).unwrap_or(false)
        && git_command_success(workspace, &["remote", "get-url", "origin"]).unwrap_or(false)
    {
        let _ = git_command_success(
            workspace,
            &["fetch", "--no-tags", "--quiet", "origin", head_branch],
        );
    }
    let parents = git_output(workspace, &["rev-list", "--parents", "-n", "1", head])
        .map_err(|error| format!("head {head} cannot be read: {error}"))?;
    let parents: Vec<&str> = parents.split_whitespace().skip(1).collect();
    let [first, second] = parents.as_slice() else {
        return Err(format!("head {head} is not a two-parent merge"));
    };
    let base_side = match (*first == candidate, *second == candidate) {
        (true, false) => *second,
        (false, true) => *first,
        _ => return Err(format!("head {head} does not merge candidate {candidate}")),
    };
    if !contains(workspace, base_side, landing_ref) {
        return Err(format!(
            "head {head} merges {base_side}, which landing ref '{landing_ref}' does not contain"
        ));
    }
    let merged = git_output(
        workspace,
        &["merge-tree", "--write-tree", candidate, base_side],
    )
    .map_err(|error| {
        format!("candidate {candidate} does not merge cleanly with {base_side}: {error}")
    })?;
    let tree = git_output(workspace, &["rev-parse", &format!("{head}^{{tree}}")])
        .map_err(|error| format!("head {head} has no readable tree: {error}"))?;
    if merged.lines().next().map(str::trim) != Some(tree.trim()) {
        return Err(format!(
            "head {head} changes more than the merge of candidate {candidate} with {base_side}"
        ));
    }
    Ok(())
}

/// Shared owner retry/landing proof for a provider base-only update. A cached
/// base never stands in for a successful fetch. Source and native evidence
/// continue to describe the original immutable accepted candidate.
pub(super) fn verify_published_base_update(
    workspace: &Path,
    candidate: &orbit_types::workflow::handoff::HandoffCandidate,
    status: &PullRequestStatus,
) -> Result<(), OrbitError> {
    let refuse =
        |reason: String| OrbitError::Execution(format!("provider base update refused: {reason}"));
    let orbit_types::workflow::handoff::HandoffDelivery::PullRequest { number } =
        candidate.delivery
    else {
        return Err(refuse("published delivery required".into()));
    };
    super::pr::DeliveryPin::pinned(
        &candidate.source_branch,
        &candidate.base_branch,
        &candidate.candidate.commit,
    )
    .ensure_candidate_identity(status, &number.to_string())?;
    if status.number != Some(number)
        || status.url.as_deref()
            != Some(&format!(
                "https://github.com/{}/pull/{number}",
                candidate.repository
            ))
    {
        return Err(refuse("repository or PR changed".into()));
    }
    let head = status
        .head_sha
        .as_deref()
        .ok_or_else(|| refuse("head unavailable".into()))?;
    if !matches!(head.len(), 40 | 64) || !head.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(refuse(
            "provider head is not an exact commit identity".into(),
        ));
    }
    let remote = git_command_success(workspace, &["remote", "get-url", "origin"])?;
    let landing = if remote {
        super::git::fetch_remote_base(workspace, &candidate.landing_branch)?;
        format!("origin/{}", candidate.landing_branch)
    } else {
        candidate.landing_branch.clone()
    };
    if !contains(workspace, &candidate.base.commit, &landing) {
        return Err(refuse("landing base rewound or unrelated".into()));
    }
    verified_base_update(
        workspace,
        &candidate.candidate.commit,
        head,
        &candidate.source_branch,
        &landing,
    )
    .map_err(refuse)?;
    let parents = git_output(workspace, &["rev-list", "--parents", "-n", "1", head])?;
    let base_side = parents
        .split_whitespace()
        .skip(1)
        .find(|parent| *parent != candidate.candidate.commit)
        .ok_or_else(|| refuse("base parent unavailable".into()))?;
    if !contains(workspace, &candidate.base.commit, base_side)
        || contains(workspace, base_side, &candidate.candidate.commit)
    {
        return Err(refuse(
            "provider base parent is rewound, unrelated or has not advanced".into(),
        ));
    }
    if matches!(
        super::pr::classify_pr_state(status),
        super::pr::PrMergeState::Merged
    ) {
        let merge = status
            .merge_commit
            .as_deref()
            .ok_or_else(|| refuse("merge commit unavailable".into()))?;
        if !matches!(merge.len(), 40 | 64) || !merge.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(refuse(
                "provider merge is not an exact commit identity".into(),
            ));
        }
        if !contains(workspace, merge, &landing) {
            return Err(refuse(
                "reported merge is absent from the fetched landing base".into(),
            ));
        }
        let parents = git_output(workspace, &["rev-list", "--parents", "-n", "1", merge])?;
        let parent = parents
            .split_whitespace()
            .nth(1)
            .ok_or_else(|| refuse("merge parent unavailable".into()))?;
        let clean = git_output(workspace, &["merge-tree", "--write-tree", head, parent])?;
        let tree = git_output(workspace, &["rev-parse", &format!("{merge}^{{tree}}")])?;
        if clean.lines().next().map(str::trim) != Some(tree.trim()) {
            return Err(refuse(
                "reported merge tree does not deliver this provider head".into(),
            ));
        }
    } else if remote {
        let actual = git_output(
            workspace,
            &[
                "ls-remote",
                "--heads",
                "origin",
                &format!("refs/heads/{}", candidate.source_branch),
            ],
        )?;
        if actual.split_whitespace().next() != Some(head) {
            return Err(refuse("remote/provider head lease changed".into()));
        }
    }
    Ok(())
}
