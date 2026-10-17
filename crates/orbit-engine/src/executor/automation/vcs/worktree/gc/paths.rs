use super::super::super::git::{git_command_success, git_output};
use super::super::{WorktreeIdentity, resolve_shared_worktree_path};
use super::sweep::{WorktreeGcOptions, WorktreeGcReport};
use orbit_common::OrbitError;
use orbit_types::workflow::JobRun;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

/// A run whose stored token cannot name a directory. Setup never created one
/// for it, so the report has no path; the sweep continues and the entry stays
/// visible until the record is pruned.
pub(super) fn unresolvable_run_report(run: &JobRun, error: &OrbitError) -> WorktreeGcReport {
    let task_ids = attributed_task_ids(run);
    WorktreeGcReport {
        path: PathBuf::new(),
        run_id: Some(run.run_id.clone()),
        run_state: Some(run.state),
        task_id: (!task_ids.is_empty()).then(|| task_ids.join(",")),
        task_status: None,
        pr_status: None,
        action: format!("failed:{error}"),
        bytes_reclaimed: 0,
        detail: None,
        reclaim: Vec::new(),
        recovery: None,
        retained_bytes: None,
    }
}

/// Every directory this run could have left behind, primary path first.
///
/// The identity is re-derived with the same rule `setup_worktree` used
/// (ORB-10427) — never re-spelled here. A run whose input names no task never
/// reached `setup_worktree`; its worktree, if any, is the shared batch
/// worktree keyed by run id.
pub fn run_worktree_paths(repo_root: &Path, run: &JobRun) -> Result<Vec<PathBuf>, OrbitError> {
    let input = run.input.as_ref().unwrap_or(&Value::Null);
    let Ok(identity) = WorktreeIdentity::from_input(input, Some(&run.run_id)) else {
        return Ok(vec![resolve_shared_worktree_path(repo_root, &run.run_id)?]);
    };
    let mut paths = vec![identity.path(repo_root)?];
    paths.extend(identity.fallback_path(repo_root)?);
    Ok(paths)
}

/// The tasks a run's worktree serves, empty when the run names none.
pub(super) fn attributed_task_ids(run: &JobRun) -> Vec<String> {
    WorktreeIdentity::from_input(
        run.input.as_ref().unwrap_or(&Value::Null),
        Some(&run.run_id),
    )
    .map(|identity| identity.task_ids)
    .unwrap_or_default()
}

pub(super) fn on_disk_worktrees(repo_root: &Path) -> Result<Vec<PathBuf>, OrbitError> {
    let sentinel = resolve_shared_worktree_path(repo_root, "gc-root-sentinel")?;
    let Some(root) = sentinel.parent() else {
        return Ok(Vec::new());
    };
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for entry in fs::read_dir(root).map_err(|error| {
        OrbitError::Execution(format!(
            "failed to inventory worktree root '{}': {error}",
            root.display()
        ))
    })? {
        let entry = entry.map_err(|error| {
            OrbitError::Execution(format!(
                "failed to read an entry under '{}': {error}",
                root.display()
            ))
        })?;
        if entry
            .file_type()
            .map_err(|error| {
                OrbitError::Execution(format!(
                    "failed to inspect '{}': {error}",
                    entry.path().display()
                ))
            })?
            .is_dir()
        {
            paths.push(entry.path());
        }
    }
    Ok(paths)
}

pub(super) fn branch_name(worktree: &Path) -> Result<String, OrbitError> {
    let branch = git_output(worktree, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let branch = branch.trim();
    if branch.is_empty() || branch == "HEAD" {
        return Err(OrbitError::Execution(format!(
            "cannot safely collect detached worktree '{}'",
            worktree.display()
        )));
    }
    Ok(branch.to_string())
}

/// Why Git holds this worktree locked (`git worktree lock`), if it does. The
/// lock is the hold an operator or tool can take on a run checkout; Orbit
/// itself takes none. Git refuses to remove a locked worktree, with or
/// without a single `--force`, so a lock taken after this check still keeps
/// it. A lock file GC cannot read counts as held.
pub(super) fn worktree_lock(worktree: &Path) -> Result<Option<String>, OrbitError> {
    let admin_dir = git_output(worktree, &["rev-parse", "--absolute-git-dir"])?;
    match fs::read_to_string(Path::new(&admin_dir).join("locked")) {
        Ok(reason) if reason.trim().is_empty() => Ok(Some("Git holds this worktree locked".into())),
        Ok(reason) => Ok(Some(format!(
            "Git holds this worktree locked: {}",
            reason.trim()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Ok(Some(format!(
            "cannot read the worktree's Git lock: {error}"
        ))),
    }
}

pub(super) fn branch_exists(repo_root: &Path, branch: &str) -> bool {
    git_command_success(
        repo_root,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .unwrap_or(false)
}

/// The size of a kept directory, when the sweep deletes or estimates. A walk
/// that fails (the directory vanished or is unreadable) reports no size.
pub(super) fn retained_bytes(path: &Path, options: &WorktreeGcOptions) -> Option<u64> {
    if !(options.delete || options.estimate_bytes) {
        return None;
    }
    fs::symlink_metadata(path)
        .is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink())
        .then(|| directory_bytes(path).ok())
        .flatten()
}

pub(super) fn directory_bytes(path: &Path) -> Result<u64, OrbitError> {
    let mut total = 0u64;
    let mut pending = vec![path.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in fs::read_dir(&current).map_err(|error| {
            OrbitError::Execution(format!(
                "failed to measure worktree '{}': {error}",
                current.display()
            ))
        })? {
            let entry = entry.map_err(|error| {
                OrbitError::Execution(format!(
                    "failed to measure an entry under '{}': {error}",
                    current.display()
                ))
            })?;
            let metadata = fs::symlink_metadata(entry.path()).map_err(|error| {
                OrbitError::Execution(format!(
                    "failed to measure '{}': {error}",
                    entry.path().display()
                ))
            })?;
            total = total.saturating_add(metadata.len());
            if metadata.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok(total)
}
