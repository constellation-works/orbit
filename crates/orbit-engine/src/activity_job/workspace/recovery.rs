use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use super::declared_pair::canonical_git_common_dir;
use super::fingerprint::{DIFF_IDENTITY_FLAGS, GitWorktreeFingerprint, git_stdout_bytes};
use super::{DispatchError, WorktreeBoundaryGuard};

/// Attempt payloads kept per run; the oldest are pruned past this.
const MAX_ATTEMPTS_PER_RUN: usize = 4;
/// Runs whose recovery payloads are kept; the least recently written run
/// directories are pruned past this.
const MAX_RETAINED_RUNS: usize = 16;
const ATTEMPT_PREFIX: &str = "attempt-";

/// Durable, content-bearing evidence written before a dirty integrity failure
/// can reach worktree cleanup. Every failure writes its own `attempt`.
#[derive(Debug, Clone, Serialize)]
pub(super) struct WorktreeRecoveryArtifact {
    attempt: u32,
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
    attempt: u32,
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

        let common_dir =
            canonical_git_common_dir(&self.assigned_root).map_err(|error| match error {
                DispatchError::CliInvocationPermanent(reason) => {
                    DispatchError::CliInvocationPermanent(format!(
                        "cannot preserve dirty worktree '{}': Git common dir is unavailable: {reason}",
                        self.assigned_root.display()
                    ))
                }
                other => other,
            })?;
        let recovery_parent = common_dir.join("orbit").join("worktree-recovery");
        let run_root = recovery_parent.join(&self.run_id);
        fs::create_dir_all(&run_root)
            .map_err(|error| recovery_io_error("create run recovery root", &run_root, error))?;
        // Each dirty failure keeps its own payload: a later failure in the
        // same run must not be answered with an earlier, staler snapshot.
        let mut attempt = existing_attempts(&run_root)
            .last()
            .map_or(1, |(latest, _)| latest + 1);
        let artifact = loop {
            let recovery_root = run_root.join(attempt_dir_name(attempt));
            let pending = recovery_parent.join(format!(
                ".{}.{}.{}.pending",
                self.run_id,
                std::process::id(),
                nanos_since_epoch()
            ));
            fs::create_dir(&pending)
                .map_err(|error| recovery_io_error("create pending recovery", &pending, error))?;
            if let Err(error) = self.write_recovery_payload(&pending, assigned_after, attempt) {
                // Best effort: never leave a half-written payload behind.
                let _ = fs::remove_dir_all(&pending);
                return Err(error);
            }
            match fs::rename(&pending, &recovery_root) {
                Ok(()) => {
                    break WorktreeRecoveryArtifact {
                        attempt,
                        tracked_patch: recovery_root.join("tracked.patch"),
                        untracked_payload: recovery_root.join("untracked"),
                        manifest: recovery_root.join("manifest.json"),
                        root: recovery_root,
                    };
                }
                Err(_) if recovery_root.exists() => {
                    // A concurrent writer took this attempt number; take the next.
                    let _ = fs::remove_dir_all(&pending);
                    attempt += 1;
                }
                Err(error) => {
                    let _ = fs::remove_dir_all(&pending);
                    return Err(recovery_io_error(
                        "publish worktree recovery",
                        &recovery_root,
                        error,
                    ));
                }
            }
        };
        prune_recovery(&recovery_parent, &self.run_id);
        Ok(Some(artifact))
    }

    /// Fill a pending recovery directory: the tracked patch, a copy of every
    /// untracked path, and the manifest naming them.
    fn write_recovery_payload(
        &self,
        pending: &Path,
        assigned_after: &GitWorktreeFingerprint,
        attempt: u32,
    ) -> Result<(), DispatchError> {
        let pending_payload = pending.join("untracked");
        fs::create_dir(&pending_payload).map_err(|error| {
            recovery_io_error("create untracked recovery payload", &pending_payload, error)
        })?;

        let mut diff_args = vec!["diff"];
        diff_args.extend(DIFF_IDENTITY_FLAGS);
        diff_args.extend(["HEAD", "--"]);
        let patch = git_stdout_bytes(&self.assigned_root, &diff_args)?;
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
            attempt,
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

fn attempt_dir_name(attempt: u32) -> String {
    format!("{ATTEMPT_PREFIX}{attempt:04}")
}

fn nanos_since_epoch() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos())
}

/// Published attempt directories of one run, oldest first.
fn existing_attempts(run_root: &Path) -> Vec<(u32, PathBuf)> {
    let Ok(entries) = fs::read_dir(run_root) else {
        return Vec::new();
    };
    let mut attempts: Vec<(u32, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let number = name.to_str()?.strip_prefix(ATTEMPT_PREFIX)?.parse().ok()?;
            Some((number, entry.path()))
        })
        .collect();
    attempts.sort();
    attempts
}

/// Bound the recovery store after a successful write: keep the newest
/// attempts of the current run and the most recently written runs. The
/// payload just published is never pruned. Best effort: a failed removal only
/// leaves the store larger and must not mask the integrity diagnostic.
fn prune_recovery(recovery_parent: &Path, current_run: &str) {
    let attempts = existing_attempts(&recovery_parent.join(current_run));
    let excess = attempts.len().saturating_sub(MAX_ATTEMPTS_PER_RUN);
    for (_, stale) in attempts.into_iter().take(excess) {
        remove_pruned(&stale);
    }

    let Ok(entries) = fs::read_dir(recovery_parent) else {
        return;
    };
    // Runs are ordered by their directory mtime, which advances whenever an
    // attempt is published or pruned. Dot-prefixed entries are in-flight
    // pending payloads, not runs.
    let mut runs: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            name != current_run && !name.to_string_lossy().starts_with('.')
        })
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, entry.path()))
        })
        .collect();
    runs.sort();
    // The current run occupies one of the retained slots.
    let excess = runs.len().saturating_sub(MAX_RETAINED_RUNS - 1);
    for (_, stale) in runs.into_iter().take(excess) {
        remove_pruned(&stale);
    }
}

fn remove_pruned(path: &Path) {
    if let Err(error) = fs::remove_dir_all(path) {
        tracing::warn!(
            path = %path.display(),
            %error,
            "failed to prune worktree recovery payload"
        );
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
/// dangling or directory link must not fail preservation. A directory,
/// including an untracked nested repository, is copied as a tree; links
/// inside it are recreated rather than followed.
fn copy_untracked_entry(source: &Path, destination: &Path) -> std::io::Result<()> {
    let file_type = fs::symlink_metadata(source)?.file_type();
    if file_type.is_symlink() {
        let target = fs::read_link(source)?;
        #[cfg(unix)]
        {
            return std::os::unix::fs::symlink(target, destination);
        }
        #[cfg(not(unix))]
        {
            // Record the link target as text rather than follow it.
            return fs::write(destination, target.to_string_lossy().as_bytes());
        }
    }
    if file_type.is_dir() {
        fs::create_dir(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_untracked_entry(&entry.path(), &destination.join(entry.file_name()))?;
        }
        return Ok(());
    }
    fs::copy(source, destination).map(|_| ())
}

fn recovery_io_error(action: &str, path: &Path, error: std::io::Error) -> DispatchError {
    DispatchError::CliInvocationPermanent(format!("{action} at '{}': {error}", path.display()))
}
