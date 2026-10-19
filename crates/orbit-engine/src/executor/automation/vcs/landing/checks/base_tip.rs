//! Known-red evidence from the base tip and its latest push jobs.

use std::path::Path;

use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::super::super::ci::log_signature::error_signature;
use super::super::super::git::git_output;
use super::super::super::operations;

use super::base_update::contains;
use super::jobs::{
    answers, commit_jobs, excerpt, failed_step, single_completed_job, transient_evidence,
    unsuccessful,
};
use super::log::job_log;

/// Compare every required failure with the latest push jobs on the exact
/// current base tip. A green tip needs no historical fork-point logs. A red
/// tip needs the same emitted diagnostic; sharing a job name alone is not
/// enough to suppress a candidate repair.
///
/// A green tip is not proof that the base fixed that one failure. When the
/// latest push run of the head's fork point with that tip passed the same
/// job, the failure is the candidate's: that check adds no refresh evidence
/// and does not end the comparison. A required sibling that fails on the tip
/// with the same diagnostic is still a known-red base, and it wins. The
/// result lists only those known-red checks.
///
/// The result is `None`, so the ordinary repair path decides and no base
/// refresh is recorded, when no check is known-red and any of these holds:
/// every required failure was candidate-owned, at least one was (a green-tip
/// sibling must not spend the base update, including when that update is
/// already reserved), or the tip is already contained in the head. An
/// unreadable or missing fork-point run on a green tip that no
/// candidate-owned check accompanies keeps the green-tip refresh. A pending
/// tip job, a tip conclusion other than success or failure, a different
/// diagnostic, or a transient log still returns `None` for the whole call.
pub(in crate::executor::automation::vcs) fn base_tip_check_evidence<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    head: &str,
    base_branch: &str,
    tip: &str,
    failed: &[(Option<String>, String)],
) -> Option<Value> {
    let jobs = commit_jobs(host, workspace, head).ok()?;
    let tip_jobs = latest_push_jobs(host, workspace, tip, base_branch)?;
    let mut fork_point_jobs = None;
    let mut evidence = Vec::new();
    let mut red = false;
    // A check the fork point and the tip both passed is the candidate's.
    // Remember it and keep reading. Returning here dropped a known-red
    // sibling, and completion then spent a repair on that inherited failure.
    let mut candidate_owned = false;
    for check in failed {
        let job = jobs
            .iter()
            .find(|job| unsuccessful(job) && answers(job, check))?;
        // Ambiguous workflow names, incomplete jobs and pending jobs prove
        // neither a passing base nor an inherited failure.
        let base_job = single_completed_job(&tip_jobs, job)?;
        if base_job["conclusion"] == "success" {
            let fork_point_jobs = fork_point_jobs.get_or_insert_with(|| {
                git_output(workspace, &["merge-base", head, tip])
                    .ok()
                    .and_then(|fork_point| {
                        latest_push_jobs(host, workspace, fork_point.trim(), base_branch)
                    })
            });
            if fork_point_jobs
                .as_deref()
                .and_then(|fork_point_jobs| single_completed_job(fork_point_jobs, job))
                .is_some_and(|fork_point_job| fork_point_job["conclusion"] == "success")
            {
                candidate_owned = true;
                continue;
            }
            evidence.push(json!({"failed_job":job,"passing_job":base_job}));
            continue;
        }
        if base_job["conclusion"] != "failure" {
            return None;
        }
        let log = job_log(
            host,
            workspace,
            job["run_id"].as_u64()?,
            job["job_id"].as_u64()?,
            job,
        )
        .ok()?;
        let base_log = job_log(
            host,
            workspace,
            base_job["run_id"].as_u64()?,
            base_job["job_id"].as_u64()?,
            base_job,
        )
        .ok()?;
        if transient_evidence(&log).is_some() || transient_evidence(&base_log).is_some() {
            return None;
        }
        let signature = error_signature(&log, failed_step(job));
        let base_signature = error_signature(&base_log, failed_step(base_job));
        if signature.step_fallback
            || base_signature.step_fallback
            || signature.text != base_signature.text
        {
            return None;
        }
        red = true;
        evidence.push(json!({"failed_job":job,"base_job":base_job,"signature":signature.text,"log_excerpt":excerpt(&log)}));
    }
    // No known-red check, and either nothing to refresh or a candidate-owned
    // failure that must keep the ordinary repair. A green-tip sibling alone
    // would be a base refresh; beside a candidate-owned check it is not.
    if evidence.is_empty() || (!red && (candidate_owned || contains(workspace, tip, head))) {
        return None;
    }
    Some(
        json!({"failed_head":head,"landed_base":tip,"base_branch":base_branch,"source":"latest_push","checks":evidence,
        "path":if red {"known_red_base"} else {"base_refresh"}}),
    )
}

/// The jobs of the latest push run of each workflow on `commit` of
/// `base_branch`; `None` when they could not be read.
fn latest_push_jobs<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    commit: &str,
    base_branch: &str,
) -> Option<Vec<Value>> {
    let response = host
        .run_private_vcs_operation(
            operations::CI_COMMIT_JOBS,
            json!({"commit":commit,"workspace_path":workspace,"latest_push_branch":base_branch}),
        )
        .ok()?;
    response["jobs"].as_array().cloned()
}
