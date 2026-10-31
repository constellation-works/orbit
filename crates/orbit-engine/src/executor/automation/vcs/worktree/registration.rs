use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;

use super::super::git::git_output;

/// Registered worktree paths for `repo_root`, as Git recorded them and in
/// canonical form when the directory still exists.
///
/// Match on canonical paths, not raw strings. `git worktree list` reports the
/// resolved path, while the caller holds whatever path it was handed. Where the
/// two differ only by a symlink on the way down — on macOS `/var` and `/tmp`
/// are symlinks into `/private`, so any worktree under them reports one path
/// and is asked about under another — a literal comparison reads a registered
/// worktree as unregistered and GC retains it forever.
///
/// The literal comparison is kept as the fast path, and a registered entry
/// whose directory has already been removed simply fails to canonicalize and
/// does not match, which is the same answer the literal comparison gave.
pub(in crate::executor::automation::vcs) fn registered_worktree_paths(
    repo_root: &Path,
) -> Result<BTreeSet<PathBuf>, OrbitError> {
    let list = git_output(repo_root, &["worktree", "list", "--porcelain"])?;
    let mut registered = BTreeSet::new();
    for line in list.lines() {
        let Some(recorded) = line.strip_prefix("worktree ") else {
            continue;
        };
        let path = PathBuf::from(recorded);
        if let Ok(canonical) = std::fs::canonicalize(&path) {
            registered.insert(canonical);
        }
        registered.insert(path);
    }
    Ok(registered)
}

/// Whether `path` is in a set produced by [`registered_worktree_paths`].
pub(in crate::executor::automation::vcs) fn path_is_registered(
    registered: &BTreeSet<PathBuf>,
    path: &Path,
) -> bool {
    if registered.contains(path) {
        return true;
    }
    std::fs::canonicalize(path).is_ok_and(|canonical| registered.contains(&canonical))
}

/// Whether `path` is one of `repo_root`'s registered worktrees (main or
/// linked), comparing both the literal path Git recorded and its canonical
/// form. Any other checkout that happens to live at `path` is not ours to
/// clean or remove.
pub(in crate::executor::automation::vcs) fn is_registered_worktree(
    repo_root: &Path,
    path: &Path,
) -> Result<bool, OrbitError> {
    Ok(path_is_registered(
        &registered_worktree_paths(repo_root)?,
        path,
    ))
}
