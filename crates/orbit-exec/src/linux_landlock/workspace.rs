//! Compiling the resolved read profile into workspace path grants.
//!
//! The profile is a last-match-wins list of globs; a Landlock ruleset is a set
//! of inodes. Translating one into the other means walking the workspace once
//! and deciding, per path, whether the child may read it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_types::policy::{CompiledFsRules, FsOperation, ResolvedFsProfile};

use super::LandlockPathGrant;

/// Compile the workspace half of the ruleset for `profile`.
pub(super) fn workspace_read_grants(
    workspace_root: &Path,
    profile: &ResolvedFsProfile,
) -> Result<Vec<LandlockPathGrant>, OrbitError> {
    let rules = profile.compile(FsOperation::Read)?;
    if rules.grants_nothing() {
        return Ok(Vec::new());
    }

    if rules.allows(".")? {
        return grant_read_tree(workspace_root, workspace_root, &rules);
    }
    allowed_subtrees(workspace_root, workspace_root, &rules)
}

/// Grant `root` as a readable tree, carving out every denied path beneath it.
///
/// A directory holding a denied path cannot be granted as a tree, because a
/// path-beneath rule reaches every descendant. Such a directory is granted
/// list-only and each allowed child is granted in its own right, recursively.
/// The denied path is then left with no readable ancestor at all.
///
/// # What this does and does not deny
/// The carve-out is computed from the paths that exist when the ruleset is
/// compiled, and Landlock rules bind to inodes. Consequences, all covered by
/// tests:
///
/// - A denied file that exists at spawn stays unreadable for the child's whole
///   life, including after the child renames it within its directory: the
///   inode keeps no readable ancestor.
/// - The child cannot move a denied file into a readable directory. Landlock
///   refuses a rename that would give a file more access at its destination,
///   which is why `REFER` is handled.
/// - A file matching a deny rule that is *created* later under an
///   already-granted directory is readable by that child. That discloses
///   nothing the child could not already obtain — either the child wrote those
///   bytes, or it copied them from somewhere this ruleset already allowed.
///   A concurrent third party writing a new secret into the workspace during
///   the child's lifetime is the residual race, and it is why `denyRead` is
///   also enforced at request time by the policy engine.
pub(super) fn grant_read_tree(
    workspace_root: &Path,
    root: &Path,
    rules: &CompiledFsRules,
) -> Result<Vec<LandlockPathGrant>, OrbitError> {
    let denied = denied_paths(workspace_root, root, rules)?;
    carve_out(root, &denied)
}

/// Walk into a workspace whose root is not itself allowed, granting the
/// allowed subtrees found along the way.
fn allowed_subtrees(
    workspace_root: &Path,
    dir: &Path,
    rules: &CompiledFsRules,
) -> Result<Vec<LandlockPathGrant>, OrbitError> {
    let mut grants = Vec::new();
    for child in children_under(workspace_root, dir)? {
        if rules.allows(&relative_to(workspace_root, &child)?)? {
            grants.extend(grant_read_tree(workspace_root, &child, rules)?);
        } else if child.is_dir() {
            grants.extend(allowed_subtrees(workspace_root, &child, rules)?);
        }
    }
    Ok(grants)
}

/// Every path beneath `root` the profile denies. Empty when the profile has no
/// exclusion, in which case no walk is needed at all.
fn denied_paths(
    workspace_root: &Path,
    root: &Path,
    rules: &CompiledFsRules,
) -> Result<BTreeSet<PathBuf>, OrbitError> {
    let mut denied = BTreeSet::new();
    if rules.has_exclusion() {
        collect_denied(workspace_root, root, rules, &mut denied)?;
    }
    Ok(denied)
}

fn collect_denied(
    workspace_root: &Path,
    dir: &Path,
    rules: &CompiledFsRules,
    denied: &mut BTreeSet<PathBuf>,
) -> Result<(), OrbitError> {
    for child in children_under(workspace_root, dir)? {
        if !rules.allows(&relative_to(workspace_root, &child)?)? {
            denied.insert(child);
        } else if child.is_dir() {
            collect_denied(workspace_root, &child, rules, denied)?;
        }
    }
    Ok(())
}

/// Grant `root` while leaving every path in `denied` without a readable
/// ancestor. Shared with the host grants, which carve credential files out of
/// an otherwise readable tool state directory the same way.
pub(super) fn carve_out(
    root: &Path,
    denied: &BTreeSet<PathBuf>,
) -> Result<Vec<LandlockPathGrant>, OrbitError> {
    if denied.contains(root) {
        return Ok(Vec::new());
    }
    let holds_denied_path = denied.iter().any(|path| path.starts_with(root));
    if !holds_denied_path {
        return Ok(vec![whole_path_grant(root)]);
    }
    if !root.is_dir() {
        return Ok(Vec::new());
    }

    let mut grants = vec![LandlockPathGrant::list_only(root.to_path_buf())];
    for child in children_under(root, root)? {
        grants.extend(carve_out(&child, denied)?);
    }
    Ok(grants)
}

fn whole_path_grant(path: &Path) -> LandlockPathGrant {
    if path.is_dir() {
        LandlockPathGrant::read_tree(path.to_path_buf())
    } else {
        LandlockPathGrant::read_file(path.to_path_buf())
    }
}

/// Canonical children of `dir`, dropping any entry that resolves outside
/// `boundary`. A symlink pointing out of the workspace must not smuggle an
/// outside path into the ruleset; the child following that link is denied at
/// the resolved location, which is where the profile is defined.
///
/// This runs once per directory before every activity-scoped spawn, so it
/// avoids per-entry syscalls: `dir` is already canonical, which makes a
/// non-symlink child canonical by construction, and the entry kind comes from
/// the directory read itself. Only a symlink pays for full resolution.
///
/// An unreadable directory yields nothing rather than failing the whole
/// compilation: the child could not have read it either.
fn children_under(boundary: &Path, dir: &Path) -> Result<Vec<PathBuf>, OrbitError> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(Vec::new());
    };
    let mut children = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            OrbitError::Io(format!("read landlock path `{}`: {error}", dir.display()))
        })?;
        let symlinked = entry.file_type().is_ok_and(|kind| kind.is_symlink());
        let path = if symlinked {
            let Ok(resolved) = entry.path().canonicalize() else {
                continue;
            };
            resolved
        } else {
            entry.path()
        };
        if path.starts_with(boundary) {
            children.push(path);
        }
    }
    Ok(children)
}

fn relative_to(workspace_root: &Path, path: &Path) -> Result<String, OrbitError> {
    let relative = path.strip_prefix(workspace_root).map_err(|_| {
        OrbitError::InvalidInput(format!(
            "landlock path `{}` escaped workspace `{}`",
            path.display(),
            workspace_root.display()
        ))
    })?;
    if relative.as_os_str().is_empty() {
        Ok(".".to_string())
    } else {
        Ok(relative.to_string_lossy().replace('\\', "/"))
    }
}
