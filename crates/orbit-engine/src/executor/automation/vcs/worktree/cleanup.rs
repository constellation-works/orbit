use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use orbit_common::OrbitError;
use orbit_exec::{EnvironmentMode, ExecRequest, NoSandbox, StdinMode, run_process};

use super::super::git::{git_failure_error, git_run, git_success};

/// Bounded budget for the background deletion of a relocated worktree. Not
/// the 30s git default: this runs off the critical path on its own thread, so
/// it can afford to wait out a legitimately huge `target/` without blocking
/// GC or the sweep that called it.
const TRASH_DELETE_TIMEOUT_MS: u64 = 600_000;

/// Shared sanctioned worktree removal sequence. Pipeline cleanup may force
/// removal because it owns the just-created checkout; out-of-band GC must
/// always pass `force = false`.
pub(super) fn remove_worktree(
    repo_root: &Path,
    workspace_path: &Path,
    branch_name: Option<&str>,
    force: bool,
) -> Result<(), OrbitError> {
    if workspace_path.exists() {
        if force {
            git_success(
                repo_root,
                &[
                    "worktree",
                    "remove",
                    "--force",
                    &workspace_path.to_string_lossy(),
                ],
            )?;
        } else {
            remove_worktree_without_force(repo_root, workspace_path)?;
        }
    }
    git_success(repo_root, &["worktree", "prune"])?;
    if let Some(branch_name) = branch_name {
        git_success(repo_root, &["branch", "-D", branch_name])?;
    }
    Ok(())
}

/// Remove a worktree Git has not been told to force through.
///
/// Git's own clean check runs before it deletes any file and finishes in
/// milliseconds regardless of `target/` size, because an ignored directory
/// plays no part in it — a genuinely dirty worktree is refused well inside
/// the budget. The only way this call times out is the physical delete of a
/// legitimately huge, already-clean tree, so the supervisor's timeout verdict
/// reliably tells the two cases apart: recover a timeout by relocating the
/// tree out of the way and finishing the bulk delete off the critical path,
/// but let an ordinary refusal (dirty tree, locked ref, …) propagate as-is.
pub(super) fn remove_worktree_without_force(
    repo_root: &Path,
    workspace_path: &Path,
) -> Result<(), OrbitError> {
    let path = workspace_path.to_string_lossy().into_owned();
    let args = ["worktree", "remove", path.as_str()];
    let outcome = git_run(repo_root, &args)?;
    if outcome.success {
        return Ok(());
    }
    if !outcome.timed_out {
        return Err(git_failure_error(repo_root, &args, &outcome.stderr));
    }
    relocate_worktree_to_trash(workspace_path)
}

/// Move a worktree directory to a same-filesystem sibling — an instant,
/// constant-time rename regardless of how much it contains — then delete that
/// sibling in the background. The caller prunes Git's administrative entry
/// for the now-missing path right after this returns, so metadata cleanup
/// never waits on the bulk delete, and a delete that never finishes leaves
/// only a `.trash-*` leftover behind, never a dangling worktree registration.
fn relocate_worktree_to_trash(workspace_path: &Path) -> Result<(), OrbitError> {
    let trash_path = trash_sibling_path(workspace_path)?;
    fs::rename(workspace_path, &trash_path).map_err(|error| {
        OrbitError::Execution(format!(
            "failed to relocate worktree '{}' to '{}' for background deletion: {error}",
            workspace_path.display(),
            trash_path.display()
        ))
    })?;
    spawn_trash_deletion(trash_path);
    Ok(())
}

fn trash_sibling_path(workspace_path: &Path) -> Result<PathBuf, OrbitError> {
    let parent = workspace_path.parent().ok_or_else(|| {
        OrbitError::Execution(format!(
            "worktree path '{}' has no parent directory to relocate into",
            workspace_path.display()
        ))
    })?;
    let name = workspace_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            OrbitError::Execution(format!(
                "worktree path '{}' has no usable file name to relocate",
                workspace_path.display()
            ))
        })?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    Ok(parent.join(format!(".trash-{name}-{}-{nanos}", std::process::id())))
}

/// Fire-and-forget: the caller has already pruned Git's view of this worktree
/// by the time this runs. A failure here strands a `.trash-*` directory for
/// manual or later cleanup; it never resurfaces as a GC error.
fn spawn_trash_deletion(trash_path: PathBuf) {
    std::thread::spawn(move || {
        let request = ExecRequest {
            program: "rm".to_string(),
            args: vec!["-rf".to_string(), trash_path.to_string_lossy().into_owned()],
            current_dir: None,
            timeout_ms: Some(TRASH_DELETE_TIMEOUT_MS),
            stdin_mode: StdinMode::Null,
            environment_mode: EnvironmentMode::Inherit,
            debug: false,
        };
        match run_process(&request, &NoSandbox) {
            Ok(outcome) if outcome.success => {}
            Ok(outcome) => tracing::warn!(
                path = %trash_path.display(),
                timed_out = outcome.timed_out,
                stderr = %outcome.stderr,
                "background worktree trash deletion did not finish cleanly; leftover directory needs manual cleanup"
            ),
            Err(error) => tracing::warn!(
                path = %trash_path.display(),
                %error,
                "failed to launch background worktree trash deletion; leftover directory needs manual cleanup"
            ),
        }
    });
}
