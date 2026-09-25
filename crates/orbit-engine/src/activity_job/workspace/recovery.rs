use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use super::declared_pair::git_common_dir;
use super::fingerprint::{GitWorktreeFingerprint, git_stdout_bytes};
use super::{DispatchError, WorktreeBoundaryGuard};

/// Durable, content-bearing evidence written before a dirty integrity failure
/// can reach worktree cleanup.
#[derive(Debug, Clone, Serialize)]
pub(super) struct WorktreeRecoveryArtifact {
    root: PathBuf,
    tracked_patch: PathBuf,
    untracked_payload: PathBuf,
    manifest: PathBuf,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorktreeRecoveryManifest<'a> {
    schema_version: u8,
    task_id: &'a str,
    run_id: &'a str,
    recorded_head: &'a str,
    recorded_branch: &'a Option<String>,
    tracked_patch: &'static str,
    untracked_payload: &'static str,
    untracked_files: Vec<&'a String>,
}

impl WorktreeBoundaryGuard {
    pub(super) fn preserve_dirty_assigned_worktree(
        &self,
        assigned_after: &GitWorktreeFingerprint,
    ) -> Result<Option<WorktreeRecoveryArtifact>, DispatchError> {
        // ADR-0299: content-bearing evidence must outlive forced removal of
        // the linked checkout, so it lives under the shared Git common dir.
        if assigned_after.dirty_paths.is_empty() {
            return Ok(None);
        }
        if self.run_id.is_empty()
            || !self
                .run_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(DispatchError::CliInvocationPermanent(format!(
                "cannot preserve dirty worktree for unsafe run id '{}'",
                self.run_id
            )));
        }

        let common_dir = git_common_dir(&self.assigned_root)?.ok_or_else(|| {
            DispatchError::CliInvocationPermanent(format!(
                "cannot preserve dirty worktree '{}': Git common dir is unavailable",
                self.assigned_root.display()
            ))
        })?;
        let recovery_parent = common_dir.join("orbit").join("worktree-recovery");
        let recovery_root = recovery_parent.join(&self.run_id);
        let artifact = WorktreeRecoveryArtifact {
            tracked_patch: recovery_root.join("tracked.patch"),
            untracked_payload: recovery_root.join("untracked"),
            manifest: recovery_root.join("manifest.json"),
            root: recovery_root.clone(),
        };
        if recovery_root.is_dir() {
            return Ok(Some(artifact));
        }

        fs::create_dir_all(&recovery_parent).map_err(|error| {
            recovery_io_error("create recovery parent", &recovery_parent, error)
        })?;
        let pending =
            recovery_parent.join(format!(".{}.{}.pending", self.run_id, std::process::id()));
        fs::create_dir(&pending)
            .map_err(|error| recovery_io_error("create pending recovery", &pending, error))?;
        if let Err(error) = self.write_recovery_payload(&pending, assigned_after) {
            // Best effort: never leave a half-written payload behind.
            let _ = fs::remove_dir_all(&pending);
            return Err(error);
        }
        fs::rename(&pending, &recovery_root).map_err(|error| {
            recovery_io_error("publish worktree recovery", &recovery_root, error)
        })?;
        Ok(Some(artifact))
    }

    /// Fill a pending recovery directory: the tracked patch, a copy of every
    /// untracked path, and the manifest naming them.
    fn write_recovery_payload(
        &self,
        pending: &Path,
        assigned_after: &GitWorktreeFingerprint,
    ) -> Result<(), DispatchError> {
        let pending_payload = pending.join("untracked");
        fs::create_dir(&pending_payload).map_err(|error| {
            recovery_io_error("create untracked recovery payload", &pending_payload, error)
        })?;

        let patch = git_stdout_bytes(
            &self.assigned_root,
            &[
                "diff",
                "--binary",
                "--full-index",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                "HEAD",
                "--",
            ],
        )?;
        let patch_path = pending.join("tracked.patch");
        fs::write(&patch_path, patch)
            .map_err(|error| recovery_io_error("write tracked patch", &patch_path, error))?;

        for relative in assigned_after.untracked_content.keys() {
            let relative_path = safe_relative_path(relative)?;
            let source = self.assigned_root.join(&relative_path);
            let destination = pending_payload.join(&relative_path);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(|error| {
                    recovery_io_error("create untracked payload directory", parent, error)
                })?;
            }
            copy_untracked_entry(&source, &destination).map_err(|error| {
                recovery_io_error("copy untracked recovery payload", &destination, error)
            })?;
        }

        let manifest = WorktreeRecoveryManifest {
            schema_version: 1,
            task_id: &self.task_id,
            run_id: &self.run_id,
            recorded_head: &assigned_after.head,
            recorded_branch: &assigned_after.branch,
            tracked_patch: "tracked.patch",
            untracked_payload: "untracked/",
            untracked_files: assigned_after.untracked_content.keys().collect(),
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).map_err(|error| {
            DispatchError::CliInvocationPermanent(format!(
                "serialize worktree recovery manifest for run '{}': {error}",
                self.run_id
            ))
        })?;
        let manifest_path = pending.join("manifest.json");
        fs::write(&manifest_path, manifest_bytes).map_err(|error| {
            recovery_io_error("write worktree recovery manifest", &manifest_path, error)
        })?;
        Ok(())
    }
}

pub(super) fn safe_relative_path(path: &str) -> Result<PathBuf, DispatchError> {
    let candidate = Path::new(path);
    if candidate.as_os_str().is_empty()
        || candidate
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(DispatchError::CliInvocationPermanent(format!(
            "cannot preserve unsafe untracked path '{path}'"
        )));
    }
    Ok(candidate.to_path_buf())
}

/// Copy one untracked path into a recovery payload as what it is. A symlink
/// is recreated, never followed: an agent-created link to a file outside the
/// worktree must not pull that file's contents into the payload, and a
/// dangling or directory link must not fail preservation.
fn copy_untracked_entry(source: &Path, destination: &Path) -> std::io::Result<()> {
    if !fs::symlink_metadata(source)?.file_type().is_symlink() {
        return fs::copy(source, destination).map(|_| ());
    }
    let target = fs::read_link(source)?;
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, destination)
    }
    #[cfg(not(unix))]
    {
        // Record the link target as text rather than follow it.
        fs::write(destination, target.to_string_lossy().as_bytes())
    }
}

fn recovery_io_error(action: &str, path: &Path, error: std::io::Error) -> DispatchError {
    DispatchError::CliInvocationPermanent(format!("{action} at '{}': {error}", path.display()))
}
