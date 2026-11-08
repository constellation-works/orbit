//! [ORB-23003] The landing's failed-check classification for a pull request
//! no handoff carries: an owner-local delivery left in review.
//!
//! It reads the same jobs and logs and applies the same per-job rule
//! ([`classify_logged_job`]) as [`recover_failed_checks`], and words the
//! result the same way, but it only observes. It reruns nothing, updates no
//! branch, records no base suspicion and spends no repair: there is no
//! handoff to account those against, and the watch that calls it never
//! acts on the pull request.
//!
//! [`recover_failed_checks`]: super::recover_failed_checks

use std::path::Path;

use crate::context::RuntimeHost;

use super::head::classify_logged_job;
use super::jobs::{answers, commit_jobs, describe, excerpt, job_label, unsuccessful};
use super::log::job_log;
use super::recovery::{CheckFailureClass, FailedJob};

/// The class deciding a watched pull request's failed checks, with the
/// landing's wording of every failed job.
pub(in crate::executor::automation::vcs) struct WatchedFailure {
    /// `transient`, `inherited`, `flaky`, `candidate`, `base-suspect` or
    /// `inconclusive`: the greatest class of any failed job, as the landing
    /// orders them, so one unreadable job leaves the set inconclusive.
    pub(in crate::executor::automation::vcs) class: &'static str,
    pub(in crate::executor::automation::vcs) detail: String,
}

impl WatchedFailure {
    fn inconclusive(why: String) -> Self {
        Self {
            class: CheckFailureClass::Inconclusive.as_str(),
            detail: format!("the failure was not classified ({why})"),
        }
    }
}

/// Classify the failed checks on `head`, a pull request whose fork point
/// from `base_branch` is `base`; `tip` is that branch's current commit.
#[allow(
    clippy::too_many_arguments,
    reason = "the watch passes the head, its fork point and the base tip as the landing's separate inputs"
)]
pub(in crate::executor::automation::vcs) fn classify_watched_failures<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    head: &str,
    base: &str,
    base_branch: &str,
    tip: Option<&str>,
    failed: &[(Option<String>, String)],
    deadline_ms: Option<u64>,
) -> WatchedFailure {
    let head_jobs = match commit_jobs(host, workspace, head) {
        Ok(jobs) => jobs,
        Err(error) => {
            return WatchedFailure::inconclusive(format!("its jobs could not be read: {error}"));
        }
    };
    if let Some((workflow, name)) = failed.iter().find(|check| {
        !head_jobs
            .iter()
            .any(|job| unsuccessful(job) && answers(job, check))
    }) {
        let label = workflow
            .as_ref()
            .map_or_else(|| name.clone(), |workflow| format!("{workflow} / {name}"));
        return WatchedFailure::inconclusive(format!(
            "no failed workflow job on head {head} answers check '{label}'"
        ));
    }
    let moved_tip = tip.filter(|tip| *tip != base);
    let mut base_jobs = None;
    let mut tip_jobs = None;
    let mut jobs = Vec::new();
    for job in head_jobs
        .iter()
        .filter(|job| unsuccessful(job) && failed.iter().any(|check| answers(job, check)))
    {
        let (Some(run_id), Some(job_id)) = (job.run_id.number(), job.job_id.number()) else {
            return WatchedFailure::inconclusive("a failed job reported no identity".to_string());
        };
        let label = job_label(job);
        if let Some(conclusion) = super::jobs::concluded_stopped(job) {
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
        let log = match job_log(host, workspace, run_id, job_id, job, deadline_ms) {
            Ok(log) => log,
            Err(error) => {
                return WatchedFailure::inconclusive(format!(
                    "the log of '{label}' could not be read: {error}"
                ));
            }
        };
        let (class, why, tip_passes, signature) = match classify_logged_job(
            host,
            workspace,
            job,
            &log,
            head,
            base,
            base_branch,
            moved_tip,
            &mut base_jobs,
            &mut tip_jobs,
            deadline_ms,
        ) {
            Ok(classified) => classified,
            Err(deferred) => return WatchedFailure::inconclusive(deferred.reason),
        };
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
    let Some(class) = jobs.iter().map(|job| job.class).max() else {
        return WatchedFailure::inconclusive(
            "no failed workflow job on the pull request's head answers the failed check"
                .to_string(),
        );
    };
    WatchedFailure {
        class: class.as_str(),
        detail: describe(class, &jobs),
    }
}
