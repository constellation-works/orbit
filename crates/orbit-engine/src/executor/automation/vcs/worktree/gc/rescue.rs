use super::super::super::git::git_success;
use super::super::cleanup::remove_worktree;
use super::super::rescue;
use super::leftovers::worker_may_be_alive;
use super::paths::{attributed_task_ids, branch_exists, directory_bytes};
use super::reclaim::reclaim_patterns;
use super::sweep::{WorktreeGcOptions, WorktreeGcReport};
use orbit_common::OrbitError;
use orbit_types::workflow::JobRun;
use std::path::Path;

/// Remove a settled run's dirty worktree once its candidate is recoverable:
/// a ref on `origin` holding exactly the candidate, or a verified rescue
/// patch. Whatever GC cannot capture, or a tree that changes under it, keeps
/// the worktree as a rescue candidate with the reason in `detail`.
pub(super) fn rescue_dirty_worktree(
    repo_root: &Path,
    path: &Path,
    run: &JobRun,
    branch: &str,
    options: &WorktreeGcOptions,
    mut report: WorktreeGcReport,
) -> Result<WorktreeGcReport, OrbitError> {
    let retain = |mut report: WorktreeGcReport, reason: String| {
        report.action = "skipped:dirty_rescue_candidate".to_string();
        report.detail = Some(reason);
        Ok(report)
    };
    // Forced removal must never race a worker still writing the candidate.
    if worker_may_be_alive(run) {
        report.action = "skipped:worker_alive".to_string();
        return Ok(report);
    }
    let snapshot = match rescue::candidate_snapshot(path) {
        Ok(snapshot) => snapshot,
        Err(reason) => return retain(report, reason),
    };
    let task_ids = attributed_task_ids(run);
    let recovery = match rescue::published_recovery(path, branch, &task_ids, &run.run_id, &snapshot)
    {
        Some(recovery) => recovery,
        None => match rescue::write_rescue_patch(
            repo_root,
            path,
            &run.run_id,
            branch,
            &snapshot,
            options.delete,
        ) {
            Ok(recovery) => recovery,
            Err(reason) => return retain(report, reason),
        },
    };
    let estimated_bytes = if options.delete || options.estimate_bytes {
        directory_bytes(path)?
    } else {
        0
    };
    report.bytes_reclaimed = estimated_bytes;
    report.recovery = Some(recovery);
    if !options.delete {
        report.action = "would_remove".to_string();
        return Ok(report);
    }
    // Declared build output goes first, without a Git deadline, so the forced
    // removal below has only source left to delete.
    if let Err(error) = super::super::reclaim::collect(path, reclaim_patterns(options), true) {
        tracing::warn!(path = %path.display(), %error,
            "worktree GC could not reclaim declared output before removing a rescued worktree");
    }
    // The recovery covers the tree it was checked against. Content written
    // since then is not in it, so the worktree stays.
    match rescue::candidate_snapshot(path) {
        Ok(current) if current == snapshot => {}
        _ => {
            report.bytes_reclaimed = 0;
            return retain(
                report,
                "the worktree changed while its candidate was being preserved".to_string(),
            );
        }
    }
    // `--force` is licensed only here: the candidate was just verified
    // recoverable and unchanged, and Git's dirty-tree refusal would otherwise
    // keep the checkout forever.
    remove_worktree(repo_root, path, None, true)?;
    report.action = delete_run_branch(repo_root, branch, None);
    Ok(report)
}

/// Delete a removed worktree's branch. The directory is gone and its bytes
/// are reclaimed either way; a branch that cannot be deleted right now (ref
/// lock, checked out elsewhere) is reported rather than turning the whole
/// pass into an error that hides this removal and stops the paths after it.
/// With `checked_tip`, the branch is deleted only while it still points
/// there, in one compare-and-delete, so a commit made after GC checked the
/// branch's commits keeps it.
pub(super) fn delete_run_branch(
    repo_root: &Path,
    branch: &str,
    checked_tip: Option<&str>,
) -> String {
    let deleted = |branch: &str| match checked_tip {
        Some(tip) => git_success(
            repo_root,
            &["update-ref", "-d", &format!("refs/heads/{branch}"), tip],
        ),
        None => git_success(repo_root, &["branch", "-D", branch]),
    };
    if branch_exists(repo_root, branch) && deleted(branch).is_err() {
        "removed:branch_retained".to_string()
    } else {
        "removed".to_string()
    }
}
