use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use orbit_common::OrbitError;
use orbit_exec::{EnvironmentMode, ExecRequest, NoSandbox, StdinMode, run_process};

use super::super::git::{git_failure_error, git_output_raw, git_run, git_success};

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
        // Git unregisters a worktree even when it cannot finish deleting the
        // directory, so a read-only directory left inside would strand the
        // rest unreclaimable. Repair before Git starts, so removal either
        // completes or leaves the registration intact.
        restore_owner_access(workspace_path);
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

/// Give the owner read, write and search access to every directory under
/// `root`, so a read-only directory a test fixture left behind cannot make
/// the delete fail midway.
///
/// Directories only: unlinking a file needs write access to its parent, not
/// to the file. Nothing is followed — a symlink is skipped, never
/// dereferenced, and the walk starts from `root` itself only when it is a real
/// directory, so no path outside `root` is ever touched. Best effort: a
/// directory that cannot be repaired is logged and left for the delete to
/// report.
#[cfg(unix)]
pub(super) fn restore_owner_access(root: &Path) {
    use std::os::unix::fs::PermissionsExt;

    const OWNER_RWX: u32 = 0o700;
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let metadata = match fs::symlink_metadata(&dir) {
            Ok(metadata) if metadata.is_dir() => metadata,
            Ok(_) => continue,
            Err(error) => {
                tracing::warn!(path = %dir.display(), %error, "could not inspect a directory while restoring owner access");
                continue;
            }
        };
        let mode = metadata.permissions().mode();
        if mode & OWNER_RWX != OWNER_RWX
            && let Err(error) =
                fs::set_permissions(&dir, fs::Permissions::from_mode(mode | OWNER_RWX))
        {
            tracing::warn!(path = %dir.display(), %error, "could not restore owner access to a directory before removal");
            continue;
        }
        match fs::read_dir(&dir) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                        pending.push(entry.path());
                    }
                }
            }
            Err(error) => {
                tracing::warn!(path = %dir.display(), %error, "could not list a directory while restoring owner access");
            }
        }
    }
}

#[cfg(not(unix))]
pub(super) fn restore_owner_access(_root: &Path) {}

/// Delete a directory Git no longer registers as a worktree: what an earlier
/// removal left behind when it failed partway. Git is not involved — it would
/// resolve the enclosing checkout — so the caller owns every safety gate.
pub(super) fn remove_unregistered_directory(path: &Path) -> Result<(), OrbitError> {
    restore_owner_access(path);
    // `remove_dir_all` unlinks symlinks inside the tree rather than following
    // them, so nothing outside `path` is reachable from here.
    fs::remove_dir_all(path).map_err(|error| {
        OrbitError::Execution(format!(
            "failed to remove leftover directory '{}': {error}",
            path.display()
        ))
    })
}

/// Remove a worktree Git has not been told to force through.
///
/// Git runs its refusal checks (lock, submodules, clean tree) before it
/// deletes any file, but the supervisor's timeout verdict says nothing about
/// how far Git got: the deadline can land before those checks finish just as
/// well as during the physical delete of a huge, already-approved tree. A
/// timeout alone therefore never authorizes deletion. An ordinary refusal
/// propagates as-is; a timeout goes to [`recover_timed_out_removal`], which
/// re-establishes Git's verdict independently before touching anything.
fn remove_worktree_without_force(
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
    recover_timed_out_removal(workspace_path, outcome.timeout_ms)
}

/// Finish a `git worktree remove` the supervisor killed, but only once the
/// tree is verified to be one Git would have approved (or had already begun
/// deleting). Then relocate it and finish the bulk delete off the critical
/// path. A tree that cannot be verified — dirty, locked, holding submodules,
/// or whose checks cannot complete inside the Git budget — is left in place
/// with its registration intact and reported as an error.
fn recover_timed_out_removal(workspace_path: &Path, timeout_ms: u64) -> Result<(), OrbitError> {
    if let Err(reason) = verify_interrupted_removal(workspace_path) {
        return Err(OrbitError::Execution(format!(
            "git worktree remove timed out after {timeout_ms}ms before its safety checks could be confirmed; preserved worktree '{}' in place and registered because {reason}. Rescue or inspect its contents, then remove it manually or retry with a larger git timeout",
            workspace_path.display()
        )));
    }
    relocate_worktree_to_trash(workspace_path)
}

/// Re-run Git's non-force refusal checks for a worktree whose removal was
/// interrupted. The only change tolerated is a tracked file missing from the
/// working tree with the index untouched — the footprint Git's own physical
/// delete leaves once its checks have passed, and one that loses no bytes.
fn verify_interrupted_removal(workspace_path: &Path) -> Result<(), String> {
    // Without its `.git` link, Git would resolve an enclosing repository and
    // every check below would describe the wrong tree.
    if !workspace_path.join(".git").is_file() {
        return Err("its .git link is missing, so its state cannot be verified".to_string());
    }
    let located = git_output_raw(
        workspace_path,
        &["rev-parse", "--show-toplevel", "--absolute-git-dir"],
    )
    .map_err(|error| error.to_string())?;
    let mut lines = located.lines();
    let (Some(toplevel), Some(admin_dir)) = (lines.next(), lines.next()) else {
        return Err(
            "git could not locate its working tree and administrative directory".to_string(),
        );
    };
    if !same_directory(Path::new(toplevel), workspace_path) {
        return Err(format!(
            "git resolves it to a different working tree '{toplevel}'"
        ));
    }
    let admin_dir = Path::new(admin_dir);
    if admin_dir.join("locked").exists() {
        return Err("it is locked".to_string());
    }
    if admin_dir.join("modules").is_dir() || has_populated_gitlink(workspace_path)? {
        return Err("it contains submodules".to_string());
    }
    let status = git_output_raw(
        workspace_path,
        &[
            "status",
            "--porcelain",
            "-z",
            "--ignore-submodules=none",
            "--untracked-files=all",
        ],
    )
    .map_err(|error| error.to_string())?;
    // `-z` gives a rename's source path its own field, but it trails an
    // `R`/`C` entry that already fails this check, so it is never reached.
    if let Some(entry) = status
        .split('\0')
        .find(|entry| !entry.is_empty() && !entry.starts_with(" D "))
    {
        return Err(format!(
            "it has uncommitted or untracked content ('{entry}')"
        ));
    }
    Ok(())
}

fn same_directory(left: &Path, right: &Path) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// Mirrors Git's own submodule refusal: a gitlink whose directory still has
/// content may hold history that exists nowhere else.
fn has_populated_gitlink(workspace_path: &Path) -> Result<bool, String> {
    let staged = git_output_raw(workspace_path, &["ls-files", "--stage", "-z"])
        .map_err(|error| error.to_string())?;
    Ok(staged
        .split('\0')
        .filter(|entry| entry.starts_with("160000 "))
        .filter_map(|entry| entry.split_once('\t').map(|(_, path)| path))
        .any(|path| {
            fs::read_dir(workspace_path.join(path))
                .map(|mut entries| entries.next().is_some())
                .unwrap_or(false)
        }))
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
