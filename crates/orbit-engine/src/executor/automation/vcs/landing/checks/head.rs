//! Head-check evidence for an auto-fix watcher, base inheritance and an unmoved red base.

use std::path::Path;

use serde_json::Value;

use crate::context::RuntimeHost;

use super::super::super::super::ci::log_signature::error_signature;
use super::super::super::pr::RequiredChecks;

use super::annotations::source_annotations;
use super::base_update::contains;
use super::jobs::{
    answers, commit_jobs, excerpt, failed_step, job_label, same_job, transient_evidence,
    unsuccessful,
};
use super::log::job_log;
use super::recovery::{CheckFailureClass, FailedJob};

/// What the failed checks on a pull request's head show an `--auto-fix`
/// watcher, read with the same job and log answers the landing uses.
pub(in crate::executor::automation::vcs) enum HeadCheckEvidence {
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
/// its own source failure annotations ([`annotations`](super::annotations));
/// any other check no workflow job answers (an external status, a policy
/// gate) is not. An unreadable job or log proves nothing; a log that shows
/// the runner or the network failing is not the candidate's. Each stops without a repair.
pub(in crate::executor::automation::vcs) fn head_check_evidence<H: RuntimeHost + ?Sized>(
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

/// The class of one failed job whose log was already read.
///
/// Landing and auto-fix both call this. The signature comparison is
/// [`inherited`]; nothing here re-implements it.
#[allow(clippy::too_many_arguments)]
pub(super) fn classify_logged_job<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    job: &Value,
    log: &str,
    base: &str,
    moved_tip: Option<&str>,
    base_jobs: &mut CommitJobs,
    tip_jobs: &mut CommitJobs,
) -> (CheckFailureClass, String, bool, String) {
    let signature = error_signature(log, failed_step(job));
    if let Some(evidence) = transient_evidence(log) {
        return (
            CheckFailureClass::Transient,
            format!("the log shows `{evidence}`"),
            false,
            signature.text,
        );
    }
    if signature.step_fallback {
        return (
            CheckFailureClass::Inconclusive,
            "its log names no diagnostic, only the failed step".to_string(),
            false,
            signature.text,
        );
    }
    match inherited(
        host,
        workspace,
        job,
        &signature.text,
        base,
        moved_tip,
        base_jobs,
        tip_jobs,
    ) {
        Err(gap) => (CheckFailureClass::Inconclusive, gap, false, signature.text),
        Ok(Some(found)) => (
            CheckFailureClass::Inherited,
            found.why,
            found.tip_passes,
            signature.text,
        ),
        Ok(None) => (
            CheckFailureClass::Candidate,
            format!(
                "base {base} does not fail it that way{}",
                moved_tip.map_or_else(String::new, |tip| format!(
                    " and it does not pass on the current base {tip}"
                ))
            ),
            false,
            signature.text,
        ),
    }
}

/// Whether every failed check on `head` is a workflow job the base already
/// fails the same way, and either `tip` has not moved past `base` or the
/// moved tip cannot yet prove whether the base fixed it.
///
/// `Ok(Some(reason))` is that hold: auto-fix waits and spends no repair. A
/// moved tip holds while a same job on it has not concluded, while one of
/// the failed checks has no same job there that ran to success or failure
/// (its CI skipped the job, or lists no such job yet), and while its jobs
/// cannot be read. The watcher re-reads it on the next poll, so the hold
/// ends once the tip's job concludes.
///
/// `Ok(None)` was read far enough to say this is not that hold: a
/// candidate-caused job, a runner or network failure, a check no workflow
/// job answers, or a moved tip whose same jobs all ran to success or
/// failure. A succeeded tip job is the base-refresh path, decided before
/// this one. `Err` is evidence this read could not establish: a head job
/// whose identity or log could not be read, or a classification left
/// inconclusive, such as an unreadable base job or log.
pub(in crate::executor::automation::vcs) fn unmoved_red_base<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    head: &str,
    base: &str,
    tip: &str,
    failed: &[(Option<String>, String)],
) -> Result<Option<String>, String> {
    let head_jobs = commit_jobs(host, workspace, head).map_err(|error| error.to_string())?;
    let moved_tip = (tip != base).then_some(tip);
    let mut base_jobs = None;
    let mut tip_jobs = None;
    let mut reasons = Vec::new();
    let mut inconclusive = None;
    for check in failed {
        let Some(job) = head_jobs
            .iter()
            .find(|job| unsuccessful(job) && answers(job, check))
        else {
            return Ok(None);
        };
        let (Some(run_id), Some(job_id)) = (job["run_id"].as_u64(), job["job_id"].as_u64()) else {
            return Err("a failed job reported no identity".to_string());
        };
        let label = job_label(job);
        let log = job_log(host, workspace, run_id, job_id, job)
            .map_err(|error| format!("the log of '{label}' could not be read: {error}"))?;
        let (class, why, _, _) = classify_logged_job(
            host,
            workspace,
            job,
            &log,
            base,
            moved_tip,
            &mut base_jobs,
            &mut tip_jobs,
        );
        match class {
            CheckFailureClass::Inherited => reasons.push(why),
            // A candidate-caused sibling still decides; an unread one does not.
            CheckFailureClass::Inconclusive => {
                inconclusive.get_or_insert(format!("'{label}' is inconclusive: {why}"));
            }
            _ => return Ok(None),
        }
    }
    if let Some(gap) = inconclusive {
        return Err(gap);
    }
    if reasons.is_empty() {
        return Ok(None);
    }
    // A moved tip is not the unmoved hold. It is still a hold while the tip
    // cannot prove whether the base fixed every failed check: repairing now
    // would spend an attempt on a failure the new base may already clear.
    // The landing's own branch update does not use this hold.
    if let Some(tip) = moved_tip.filter(|tip| !contains(workspace, tip, head)) {
        tip_jobs.get_or_insert_with(|| {
            commit_jobs(host, workspace, tip).map_err(|error| error.to_string())
        });
        return Ok(unproven_tip(&head_jobs, failed, &tip_jobs).map(|unproven| {
            format!(
                "base_red: {} and the base branch moved to {tip}, {unproven}, so no repair agent \
                 runs on this poll",
                reasons.join("; ")
            )
        }));
    }
    Ok(Some(format!(
        "base_red: {} and the base branch has not moved past it, so the base is still red and no \
         repair agent runs",
        reasons.join("; ")
    )))
}

/// Why the moved tip cannot prove yet whether the base fixed the failed
/// checks, or `None` when each failed check's same job on it ran to success
/// or failure.
fn unproven_tip(
    head_jobs: &[Value],
    failed: &[(Option<String>, String)],
    tip_jobs: &CommitJobs,
) -> Option<&'static str> {
    let Some(Ok(tip_jobs)) = tip_jobs.as_ref() else {
        return Some("whose jobs could not be read");
    };
    let mut unproven = None;
    for check in failed {
        let job = head_jobs
            .iter()
            .find(|job| unsuccessful(job) && answers(job, check))?;
        let same = tip_jobs
            .iter()
            .filter(|candidate| same_job(candidate, job))
            .collect::<Vec<_>>();
        if same.iter().any(|candidate| !job_concluded(candidate)) {
            return Some("whose same job has not concluded");
        }
        if !same.iter().any(|candidate| {
            matches!(
                candidate["conclusion"].as_str(),
                Some("success" | "failure")
            )
        }) {
            unproven = Some("whose same job did not run to a result there");
        }
    }
    unproven
}

/// A job GitHub has given a conclusion. An empty or absent conclusion is
/// still running, queued, or waiting.
fn job_concluded(job: &Value) -> bool {
    job["conclusion"]
        .as_str()
        .is_some_and(|conclusion| !conclusion.is_empty())
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
pub(super) fn onto_passing_tip(jobs: &[FailedJob]) -> bool {
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
