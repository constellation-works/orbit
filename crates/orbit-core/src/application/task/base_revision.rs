//! Repository reads pinned to a commit: the revision a run's worktree was cut
//! from, the workspace's current base revision, and file and directory reads
//! at a commit. Tag instructions, precedent auto-answer,
//! experiment overlays and the agent corpus share them.
//!
//! They sit in `task` so task code reads them without climbing into a higher
//! module; the run record comes through [`TaskJobPort::stored_run_state`].
//! `tag_instructions` re-exports the free functions for its older importers.

use std::path::Path;

use serde_json::Value;

use super::TaskJobPort;
use crate::OrbitRuntime;

impl OrbitRuntime {
    /// The commit the run's worktree was cut from, as its `worktree` step
    /// recorded it, or else this checkout's `HEAD` commit.
    pub(crate) fn run_base_revision(&self, run_id: &str) -> Result<String, String> {
        let recorded = self
            .stored_run_state(run_id)
            .ok()
            .flatten()
            .and_then(|state| {
                state
                    .pipeline
                    .get("worktree")
                    .and_then(|worktree| worktree.get("base_sha"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });
        match recorded {
            Some(sha) => commit_at(&self.paths().repo_root, &sha),
            None => commit_at(&self.paths().repo_root, "HEAD"),
        }
    }

    /// The workspace's current base revision: its base branch as last
    /// fetched from `origin`, else the local branch, else this checkout's
    /// `HEAD`. This is the pinned integration revision corpus injection
    /// resolves through a run's recorded base; a caller with no run (such
    /// as precedent auto-answer) pins to the same base this way, never to
    /// the live working tree.
    pub(crate) fn workspace_base_revision(&self) -> Result<String, String> {
        let root = &self.paths().repo_root;
        let branch = self.workspace_base_branch();
        let branch = branch.strip_prefix("origin/").unwrap_or(branch);
        commit_at(root, &format!("refs/remotes/origin/{branch}"))
            .or_else(|_| commit_at(root, &format!("refs/heads/{branch}")))
            .or_else(|_| commit_at(root, "HEAD"))
    }
}

/// The full commit id `revision` names in the repository at `root`.
pub(crate) fn commit_at(root: &Path, revision: &str) -> Result<String, String> {
    let repo = git2::Repository::open(root)
        .map_err(|error| format!("the repository cannot be opened: {}", error.message()))?;
    repo.revparse_single(revision)
        .and_then(|object| object.peel_to_commit())
        .map(|commit| commit.id().to_string())
        .map_err(|error| {
            format!(
                "revision {revision} cannot be resolved: {}",
                error.message()
            )
        })
}

/// The sorted repository-relative paths of the files directly under `dir`
/// in the tree of commit `revision`.
pub(crate) fn list_blob_paths_at(
    root: &Path,
    revision: &str,
    dir: &str,
) -> Result<Vec<String>, String> {
    let repo = git2::Repository::open(root)
        .map_err(|error| format!("the repository cannot be opened: {}", error.message()))?;
    let tree = repo
        .revparse_single(revision)
        .and_then(|object| object.peel_to_tree())
        .map_err(|error| format!("revision cannot be read: {}", error.message()))?;
    let entry = tree
        .get_path(Path::new(dir))
        .map_err(|_| "the directory is not in that commit".to_string())?;
    let subtree = entry
        .to_object(&repo)
        .and_then(|object| object.peel_to_tree())
        .map_err(|_| "the path is not a directory".to_string())?;
    let mut paths = Vec::new();
    for entry in subtree.iter() {
        if entry.kind() == Some(git2::ObjectType::Blob)
            && let Ok(name) = std::str::from_utf8(entry.name_bytes())
        {
            paths.push(format!("{dir}/{name}"));
        }
    }
    paths.sort();
    Ok(paths)
}

/// The bytes of `path` in the tree of commit `revision`. The admission, not
/// this read, enforces the size cap, so an oversized file is refused by name.
pub(crate) fn read_blob_at(root: &Path, revision: &str, path: &str) -> Result<Vec<u8>, String> {
    let repo = git2::Repository::open(root)
        .map_err(|error| format!("the repository cannot be opened: {}", error.message()))?;
    let tree = repo
        .revparse_single(revision)
        .and_then(|object| object.peel_to_tree())
        .map_err(|error| format!("revision cannot be read: {}", error.message()))?;
    let entry = tree
        .get_path(Path::new(path))
        .map_err(|_| "the file is not in that commit".to_string())?;
    let blob = entry
        .to_object(&repo)
        .and_then(|object| object.peel_to_blob())
        .map_err(|_| "the path is not a file".to_string())?;
    Ok(blob.content().to_vec())
}
