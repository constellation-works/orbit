//! The pinned setup checkpoint the batch commit compares HEAD against, and the
//! structured clean-tree evidence that may stand in for a new commit.

use std::path::Path;

use orbit_common::OrbitError;
use serde_json::Value;

use crate::context::RuntimeHost;

use super::super::super::input::input_string_field;
use super::super::git::{git_command_success, git_output};
use super::{already_landed, no_diff};

/// Accept a clean tree at the pinned HEAD only with structured evidence: a
/// run's no-diff claim for this task and HEAD [ORB-13145], or a verified
/// covering delivery. A stale artifact of one kind never shadows valid
/// evidence of the other; when both fail, both refusals are reported.
pub(super) fn verify_clean_tree<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &orbit_types::task::Task,
    workspace_path: &Path,
    run_id: &str,
    base_sha: &str,
) -> Result<Value, OrbitError> {
    let artifacts = host.get_task_artifacts(&task.id)?;
    let has = |path: &str| artifacts.iter().any(|artifact| artifact.path == path);
    if !has(no_diff::ARTIFACT) {
        return already_landed::verify(host, task, workspace_path, run_id, base_sha);
    }
    let no_diff_error = match no_diff::verify(host, task, workspace_path, run_id, base_sha) {
        Ok(checkpoint) => return Ok(checkpoint),
        Err(error) => error,
    };
    if !has(already_landed::ARTIFACT) {
        return Err(no_diff_error);
    }
    already_landed::verify(host, task, workspace_path, run_id, base_sha)
        .map_err(|error| OrbitError::Execution(format!("{no_diff_error}; {error}")))
}

/// The commit checkpoint a no-diff promotion or completion must recheck, when
/// the commit step accepted evidence rather than the side-effect-only tag.
pub(in crate::executor::automation::vcs) fn verified_clean_tree_checkpoint(
    input: &Value,
) -> Option<&Value> {
    input.get("already_landed_checkpoint").filter(|checkpoint| {
        matches!(
            checkpoint.get("decision").and_then(Value::as_str),
            Some(already_landed::DECISION | no_diff::DECISION)
        )
    })
}

/// Recheck a [`verified_clean_tree_checkpoint`] against live task and Git state.
pub(in crate::executor::automation::vcs) fn verify_clean_tree_handoff<H: RuntimeHost + ?Sized>(
    host: &H,
    tasks: &[orbit_types::task::Task],
    workspace_path: &Path,
    run_id: &str,
    checkpoint: &Value,
) -> Result<(), OrbitError> {
    if checkpoint.get("decision").and_then(Value::as_str) == Some(no_diff::DECISION) {
        no_diff::verify_handoff(host, tasks, workspace_path, run_id, checkpoint)
    } else {
        already_landed::verify_handoff(host, tasks, workspace_path, run_id, checkpoint)
    }
}

/// Outcome of comparing `input.base_sha` with the worktree's current HEAD.
pub(super) enum PinnedHead {
    /// No base was pinned by the caller, so this step attributes no history.
    Unpinned,
    Matched(String),
    Changed {
        base_sha: String,
        head_sha: String,
    },
}

/// Compare HEAD with the immutable checkpoint `worktree_setup` pinned for this
/// run without traversing or interpreting provider-created history.
///
/// ORB-10380: the input is a commit id resolved once at worktree creation, not
/// a ref name. `refs/remotes/origin/<base>` is shared by every worktree off the
/// one `.git`, so any sibling run's fetch or any merge moves it mid-run; a
/// commit step that re-resolved the name failed every older in-flight run by
/// construction. Nothing here resolves a ref.
pub(super) fn validate_pinned_head(
    workspace_path: &Path,
    input: &Value,
) -> Result<PinnedHead, OrbitError> {
    let Some(pinned) = input_string_field(input, "base_sha") else {
        return Ok(PinnedHead::Unpinned);
    };
    let pinned = pinned_object_id(&pinned)?;

    let base_sha = git_output(
        workspace_path,
        &["rev-parse", "--verify", &format!("{pinned}^{{commit}}")],
    )
    .map_err(|error| {
        OrbitError::Execution(format!(
            "commit_batch_changes: pinned base commit '{pinned}' is not present in worktree \
             '{}': {error}",
            workspace_path.display()
        ))
    })?;
    let head_sha = git_output(workspace_path, &["rev-parse", "--verify", "HEAD^{commit}"])?;

    if base_sha == head_sha {
        return Ok(PinnedHead::Matched(base_sha));
    }

    Ok(PinnedHead::Changed { base_sha, head_sha })
}

/// Whether HEAD is a descendant of the pinned setup checkpoint.
///
/// `validate_pinned_head` only compares object ids; it does not walk history.
/// The `no-diff-expected` tag may proceed past a moved HEAD only when that
/// HEAD sits on the pin (the run's own commits — ORB-12683). A non-descendant
/// HEAD, including an orphan/unrelated root, still fails closed (ORB-12690).
pub(super) fn head_descends_from_pin(
    workspace_path: &Path,
    base_sha: &str,
    head_sha: &str,
) -> Result<bool, OrbitError> {
    git_command_success(
        workspace_path,
        &["merge-base", "--is-ancestor", base_sha, head_sha],
    )
}

/// Reject anything that is not a full Git object id.
///
/// The commit step's contract is a base pinned at worktree setup; accepting a
/// ref name here would silently restore the moving-base failure (ORB-10380).
pub(super) fn pinned_object_id(value: &str) -> Result<String, OrbitError> {
    let candidate = value.trim();
    let is_object_id =
        matches!(candidate.len(), 40 | 64) && candidate.chars().all(|c| c.is_ascii_hexdigit());
    if !is_object_id {
        return Err(OrbitError::InvalidInput(format!(
            "git_commit: input.base_sha must be the full commit id pinned by worktree_setup, got \
             '{value}'; the commit step never resolves a ref name because the shared base ref \
             moves while a run is in flight"
        )));
    }
    Ok(candidate.to_ascii_lowercase())
}
