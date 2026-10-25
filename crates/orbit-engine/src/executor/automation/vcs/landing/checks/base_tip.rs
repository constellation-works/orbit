//! Known-red evidence from the base branch's latest push jobs, read back
//! along its first-parent history from the tip.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::super::super::ci::log_signature::error_signature;
use super::super::super::git::git_output;
use super::super::super::operations;

use super::base_update::contains;
use super::jobs::{
    answers, commit_jobs, excerpt, failed_step, same_job, transient_evidence, unsuccessful,
};
use super::log::job_log;

/// Base commits, the starting one included, read for a job that ran to a
/// result before the evidence counts as missing.
const BASE_WALK_LIMIT: usize = 8;

/// Compare every required failure with the base's latest push jobs. A green
/// base needs no historical fork-point logs. A red base needs the same
/// emitted diagnostic; sharing a job name alone is not enough to suppress a
/// candidate repair.
///
/// Each failed check is judged on the newest commit, from the current tip
/// back along the base branch's first-parent history and at most
/// [`BASE_WALK_LIMIT`] commits, whose same job ran to success or failure. A
/// commit whose CI skipped the job (a docs-only or change-scoped commit) or
/// lists no such job says nothing about it, so the read moves to its first
/// parent. A same job that has not concluded stops the read: it is the
/// answer the watcher waits for.
///
/// A green base commit is not proof that the base fixed that one failure.
/// When the head's fork point, read back the same way, also passed the same
/// job, the failure is the candidate's: that check adds no refresh evidence
/// and does not end the comparison. A required sibling that fails on the
/// base with the same diagnostic is still a known-red base, and it wins.
/// The result lists only those known-red checks. A base refresh still
/// rebases onto the tip, which contains every green commit it names, so the
/// reservation, ledger and push path stay those of a green tip.
///
/// The result is `None`, so the watcher decides, when no check is
/// known-red and any of these holds: every required failure was
/// candidate-owned, at least one was (a green-base sibling must not spend
/// the base update, including when that update is already reserved), or the
/// tip is already contained in the head. An unreadable or undecided
/// fork-point history on a green base that no candidate-owned check
/// accompanies keeps the refresh. A pending base job, no job that ran to a
/// result within the bound, an ambiguous or unreadable listing, a different
/// diagnostic, or a transient log still returns `None` for the whole call;
/// for a failure the base already fails the same way,
/// [`unmoved_red_base`](super::unmoved_red_base) then holds instead of
/// admitting a repair.
pub(in crate::executor::automation::vcs) fn base_tip_check_evidence<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    head: &str,
    base_branch: &str,
    tip: &str,
    failed: &[(Option<String>, String)],
) -> Option<Value> {
    let jobs = commit_jobs(host, workspace, head).ok()?;
    let mut history = BaseHistory {
        host,
        workspace,
        base_branch,
        jobs: HashMap::new(),
    };
    let mut fork_point = None;
    let mut evidence = Vec::new();
    let mut red = false;
    // A check the fork point and the base both passed is the candidate's.
    // Remember it and keep reading. Returning here dropped a known-red
    // sibling, and completion then spent a repair on that inherited failure.
    let mut candidate_owned = false;
    for check in failed {
        let job = jobs
            .iter()
            .find(|job| unsuccessful(job) && answers(job, check))?;
        // Ambiguous workflow names, pending jobs and a history with no
        // result prove neither a passing base nor an inherited failure.
        let (base_commit, base_job) = history.decisive(tip, job)?;
        if base_job["conclusion"] == "success" {
            let fork_point = fork_point.get_or_insert_with(|| {
                git_output(workspace, &["merge-base", head, tip])
                    .ok()
                    .map(|fork_point| fork_point.trim().to_string())
            });
            if fork_point
                .clone()
                .and_then(|fork_point| history.decisive(&fork_point, job))
                .is_some_and(|(_, fork_point_job)| fork_point_job["conclusion"] == "success")
            {
                candidate_owned = true;
                continue;
            }
            evidence
                .push(json!({"failed_job":job,"passing_job":base_job,"base_commit":base_commit}));
            continue;
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
            &base_job,
        )
        .ok()?;
        if transient_evidence(&log).is_some() || transient_evidence(&base_log).is_some() {
            return None;
        }
        let signature = error_signature(&log, failed_step(job));
        let base_signature = error_signature(&base_log, failed_step(&base_job));
        if signature.step_fallback
            || base_signature.step_fallback
            || signature.text != base_signature.text
        {
            return None;
        }
        red = true;
        evidence.push(json!({"failed_job":job,"base_job":base_job,"base_commit":base_commit,"signature":signature.text,"log_excerpt":excerpt(&log)}));
    }
    // No known-red check, and either nothing to refresh or a candidate-owned
    // failure that must keep the ordinary repair. A green-base sibling alone
    // would be a base refresh; beside a candidate-owned check it is not.
    if evidence.is_empty() || (!red && (candidate_owned || contains(workspace, tip, head))) {
        return None;
    }
    Some(
        json!({"failed_head":head,"landed_base":tip,"base_branch":base_branch,"source":"latest_push","checks":evidence,
        "path":if red {"known_red_base"} else {"base_refresh"}}),
    )
}

/// The base branch's latest push jobs, read once per commit in one call.
struct BaseHistory<'a, H: RuntimeHost + ?Sized> {
    host: &'a H,
    workspace: &'a Path,
    base_branch: &'a str,
    jobs: HashMap<String, Option<Vec<Value>>>,
}

impl<H: RuntimeHost + ?Sized> BaseHistory<'_, H> {
    /// The newest commit, from `start` back along first-parent history and
    /// at most [`BASE_WALK_LIMIT`] commits, whose `job` ran to success or
    /// failure, with that job. `None` when a same job has not concluded,
    /// a commit lists that job more than once, a listing or the history
    /// cannot be read, or no commit in the bound ran it to a result.
    fn decisive(&mut self, start: &str, job: &Value) -> Option<(String, Value)> {
        let limit = format!("--max-count={BASE_WALK_LIMIT}");
        let commits = git_output(
            self.workspace,
            &["rev-list", "--first-parent", &limit, start],
        )
        .ok()?;
        for commit in commits.split_whitespace() {
            let (host, workspace, base_branch) = (self.host, self.workspace, self.base_branch);
            let jobs = self
                .jobs
                .entry(commit.to_string())
                .or_insert_with(|| latest_push_jobs(host, workspace, commit, base_branch))
                .as_deref()?;
            let matching = jobs
                .iter()
                .filter(|candidate| same_job(candidate, job))
                .collect::<Vec<_>>();
            match matching.as_slice() {
                [] => {}
                [found] if found["status"] != "completed" => return None,
                [found] if matches!(found["conclusion"].as_str(), Some("success" | "failure")) => {
                    return Some((commit.to_string(), (*found).clone()));
                }
                [_] => {}
                _ => return None,
            }
        }
        None
    }
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
