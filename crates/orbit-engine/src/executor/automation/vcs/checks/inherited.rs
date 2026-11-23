//! [ORB-26860] Whether a head's required failures are base-inherited: red at
//! the head's fork point, passing on a newer base commit, and citing no file
//! the candidate changed. Only such a failure lets a delivery refresh its pull
//! request to the base tip.

use std::path::Path;

use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::git::git_output;
use super::base_tip::base_tip_check_evidence;
use super::head::{CheckReadGap, prerequisite_log};
use super::jobs::{answers, commit_jobs, concluded_stopped, job_label, unsuccessful};
use super::suspect::cites_changed_candidate;

/// What the base-inherited reading decided for one head.
pub(in crate::executor::automation::vcs) enum InheritedFailure {
    /// Every required failure is base-inherited; the evidence names each
    /// failed job, the passing base job and commit, and the fork point.
    Inherited(Value),
    /// The failure is not proven base-inherited, for this reason.
    NotInherited(String),
}

/// Read whether `failed`, the required failures on `head`, are inherited from
/// the base branch `base_branch` whose tip is `tip`.
///
/// Inherited means all of: [`base_tip_check_evidence`] classes the set as
/// `base_refresh` (each failed job passed on a newer base commit while the
/// head's fork point conclusively failed it, no required sibling fails on the base
/// the same way, and the tip is not already in the head), and no failed job's
/// log cites a path the candidate changed against its fork point. A stopped
/// job with no log, or a log that cannot be read, proves nothing and is not
/// inherited; a throttled read defers.
pub(in crate::executor::automation::vcs) fn base_inherited_failure<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    head: &str,
    base_branch: &str,
    tip: &str,
    failed: &[(Option<String>, String)],
    deadline_ms: Option<u64>,
) -> Result<InheritedFailure, CheckReadGap> {
    let Some(evidence) =
        base_tip_check_evidence(host, workspace, head, base_branch, tip, failed, deadline_ms)?
    else {
        return Ok(InheritedFailure::NotInherited(format!(
            "no newer commit on '{base_branch}' passes the failed required jobs that the head's \
             fork point failed"
        )));
    };
    if evidence["path"] != "base_refresh" {
        return Ok(InheritedFailure::NotInherited(format!(
            "a required job still fails on '{base_branch}' the same way, so the base has not \
             fixed it"
        )));
    }
    if !evidence["checks"].as_array().is_some_and(|checks| {
        !checks.is_empty()
            && checks
                .iter()
                .all(|check| check["fork_point_failed"] == true)
    }) {
        return Ok(InheritedFailure::NotInherited(
            "the fork point has no conclusive failing job for every required failure".to_string(),
        ));
    }
    let Ok(fork_point) = git_output(workspace, &["merge-base", head, tip]) else {
        return Ok(InheritedFailure::NotInherited(
            "the head's fork point on the base branch cannot be read".to_string(),
        ));
    };
    let fork_point = fork_point.trim().to_string();
    let Ok(jobs) = commit_jobs(host, workspace, head) else {
        return Ok(InheritedFailure::NotInherited(
            "the head's failed jobs cannot be read".to_string(),
        ));
    };
    for check in failed {
        let Some(job) = jobs
            .iter()
            .find(|job| unsuccessful(job) && answers(job, check))
        else {
            return Ok(InheritedFailure::NotInherited(format!(
                "no failed job on the head answers required check '{}'",
                check.1
            )));
        };
        let label = job_label(job);
        if concluded_stopped(job).is_some() {
            return Ok(InheritedFailure::NotInherited(format!(
                "'{label}' stopped without a log, so its failure cannot be attributed"
            )));
        }
        let (Some(run), Some(id)) = (job.run_id.number(), job.job_id.number()) else {
            return Ok(InheritedFailure::NotInherited(format!(
                "'{label}' has no complete job identity"
            )));
        };
        let log = match prerequisite_log(host, workspace, head, job, run, id, deadline_ms) {
            Ok(log) => log,
            Err(CheckReadGap::Deferred(deferred)) => return Err(CheckReadGap::Deferred(deferred)),
            Err(CheckReadGap::Unavailable(reason)) => {
                return Ok(InheritedFailure::NotInherited(reason));
            }
        };
        match cites_changed_candidate(workspace, head, &fork_point, &log) {
            Ok(false) => {}
            Ok(true) => {
                return Ok(InheritedFailure::NotInherited(format!(
                    "the failure of '{label}' cites a file the candidate changed"
                )));
            }
            Err(error) => {
                return Ok(InheritedFailure::NotInherited(format!(
                    "the files the failure of '{label}' cites cannot be compared with the \
                     candidate's changes: {error}"
                )));
            }
        }
    }
    Ok(InheritedFailure::Inherited(json!({
        "fork_point": fork_point,
        "landed_base": evidence["landed_base"],
        "checks": evidence["checks"],
    })))
}
