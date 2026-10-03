//! Observed-state diagnostics for a batch commit that has nothing to stage or
//! finds HEAD moved off its pinned checkpoint.

use std::path::Path;

use orbit_common::OrbitError;

use super::super::git::{git_output, git_output_raw};

/// The worktree carries no committable work. Reports only what was observed —
/// this message shares no wording with [`unrelated_history_error`] so a reader
/// can tell the two conditions apart (ORB-10380).
pub(super) fn empty_stage_error(
    task_id: &str,
    workspace_path: &Path,
    base_sha: Option<&str>,
) -> Result<OrbitError, OrbitError> {
    let counts = worktree_status_counts(workspace_path)?;
    let head_sha = git_output(workspace_path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let base = match base_sha {
        Some(base_sha) => format!("pinned base {base_sha}"),
        None => "no pinned base in this step's input".to_string(),
    };
    Ok(OrbitError::Execution(format!(
        "commit_batch_changes: nothing to commit for task '{task_id}' in worktree '{}'. \
         Observed after `git add --all`: {} staged, {} unstaged, {} untracked file(s); \
         HEAD {head_sha}; {base}. Orbit did not inspect, stage, or reset any other checkout",
        workspace_path.display(),
        counts.staged,
        counts.unstaged,
        counts.untracked
    )))
}

pub(super) fn changed_head_error(
    task_id: &str,
    workspace_path: &Path,
    base_sha: &str,
    head_sha: &str,
) -> OrbitError {
    OrbitError::Execution(format!(
        "commit_batch_changes: worktree_head_changed for task '{task_id}' in '{}'. \
         Observed pinned base {base_sha} and HEAD {head_sha}; the provider boundary must leave \
         the immutable setup checkpoint at HEAD. Orbit did not stage, reset, or adopt history.",
        workspace_path.display()
    ))
}

#[derive(Default)]
pub(super) struct WorktreeStatusCounts {
    pub(super) staged: usize,
    pub(super) unstaged: usize,
    pub(super) untracked: usize,
}

/// Uses [`git_output_raw`] rather than [`git_output`]: the latter trims the
/// whole output, which would eat the leading status column of a single-line
/// result (` M path` -> `M path`) and misalign the index/worktree columns by
/// one byte (see `git_output`'s doc comment).
pub(super) fn worktree_status_counts(
    workspace_path: &Path,
) -> Result<WorktreeStatusCounts, OrbitError> {
    let status = git_output_raw(
        workspace_path,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    let mut counts = WorktreeStatusCounts::default();
    for line in status.lines() {
        let mut code = line.chars();
        let (Some(index_state), Some(worktree_state)) = (code.next(), code.next()) else {
            continue;
        };
        if index_state == '?' {
            counts.untracked += 1;
            continue;
        }
        if index_state != ' ' {
            counts.staged += 1;
        }
        if worktree_state != ' ' {
            counts.unstaged += 1;
        }
    }
    Ok(counts)
}
