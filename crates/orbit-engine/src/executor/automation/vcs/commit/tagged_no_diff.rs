//! The clean-base skip of a claimed `no-diff-expected` task [ORB-14791].
//!
//! On the owner the tag alone lets a clean commit phase skip
//! (`skipped_no_diff_expected`), and `promote_no_diff` settles the task. A
//! claimed leaf has no such step: it hands off `NoDiff`, which needs a
//! checkpoint the owner can recheck. A review-only task cannot honestly write
//! `no-diff.json` or `already-landed.json`, so this checkpoint pins the tag's
//! skip to this task, run and pinned base instead. The leaf rechecks it on its
//! clean worktree, and the owner on its own copy of the task, whose tag is
//! authoritative. Claimed tagged work must not deliver a change.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::task::{NO_DIFF_EXPECTED_TAG, Task};
use serde_json::{Value, json};

use super::super::git::git_output;

/// The owner's tag-skip decision, which the claimed checkpoint shares.
pub(super) const DECISION: &str = "skipped_no_diff_expected";

/// The checkpoint a claimed tagged leaf commits to instead of a verifier
/// report. Unlike the owner's skip, it names its run and pinned base.
pub(super) fn checkpoint(task_id: &str, run_id: &str, base_sha: &str) -> Value {
    json!({
        "phase": "commit",
        "decision": DECISION,
        "committed": false,
        "skipped_no_diff_expected": true,
        "task_id": task_id,
        "job_run_id": run_id,
        "base_sha": base_sha,
    })
}

/// A claimed tagged task left a change or moved HEAD: refuse it by name rather
/// than publish code from a task that declared it would change nothing.
pub(super) fn diff_refused(
    task_id: &str,
    workspace: &Path,
    paths: usize,
    head_moved: bool,
) -> OrbitError {
    let observed = if head_moved {
        "HEAD moved off the pinned base".to_string()
    } else {
        format!("{paths} changed path(s) in the worktree")
    };
    OrbitError::Execution(format!(
        "no_diff_expected_changed: task '{task_id}' is tagged no-diff-expected and ran as a claim, \
         which hands off NoDiff and never delivers code; observed {observed} in '{}'. Orbit did \
         not stage or commit them. Revert the change, or remove the tag so the task ships as a \
         pull request",
        workspace.display()
    ))
}

/// Recheck the checkpoint on the leaf's live worktree: the task still carries
/// the tag, HEAD is the pinned base, and nothing is pending.
pub(super) fn verify_handoff(
    task: &Task,
    workspace: &Path,
    checkpoint: &Value,
) -> Result<(), OrbitError> {
    verify_handoff_at_revision(task, checkpoint)?;
    if git_output(workspace, &["rev-parse", "HEAD"])?
        != checkpoint["base_sha"].as_str().unwrap_or_default()
    {
        return Err(refused("HEAD is not the checkpoint's pinned base"));
    }
    if !git_output(
        workspace,
        &["status", "--porcelain", "--untracked-files=all"],
    )?
    .is_empty()
    {
        return Err(refused("worktree is not clean"));
    }
    Ok(())
}

/// The owner's recheck. Base equality with its live base is the caller's;
/// this asks only that the owner's copy of the task still carries the tag.
pub(super) fn verify_handoff_at_revision(
    task: &Task,
    checkpoint: &Value,
) -> Result<(), OrbitError> {
    if checkpoint["decision"] != DECISION
        || checkpoint["task_id"] != task.id.as_str()
        || checkpoint["base_sha"].as_str().is_none_or(str::is_empty)
    {
        return Err(refused(
            "checkpoint must name this task and its pinned base",
        ));
    }
    if !task.tags.iter().any(|tag| tag == NO_DIFF_EXPECTED_TAG) {
        return Err(refused(
            "the task no longer carries the no-diff-expected tag; rerun it to deliver a change",
        ));
    }
    Ok(())
}

fn refused(reason: &str) -> OrbitError {
    OrbitError::PolicyDenied(format!("no_diff_expected_unverified: {reason}"))
}
