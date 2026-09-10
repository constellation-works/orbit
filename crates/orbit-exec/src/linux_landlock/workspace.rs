//! Compiling the resolved read profile into workspace path grants.
//!
//! The profile is a last-match-wins list of globs; a Landlock ruleset is a set
//! of inodes. Translating one into the other means walking the workspace once
//! and deciding, per path, whether the child may read it.
//!
//! The translation is driven by what the exclusion *rules* can name, not by
//! which denied paths happen to exist when the walk runs. A ruleset compiled
//! from the second would hand out a whole directory whenever today's tree
//! contained no match, and a name created in it afterwards — by the child or
//! by anyone else sharing the workspace — would inherit that grant. See
//! [`DenyReach`].

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_types::policy::{CompiledFsRules, FsOperation, GlobReach, ResolvedFsProfile};

use super::LandlockPathGrant;

/// The workspace half of a compiled ruleset, and the exclusions it leaves to
/// another layer.
#[derive(Debug, Default)]
pub(super) struct WorkspaceReadGrants {
    pub(super) grants: Vec<LandlockPathGrant>,
    /// Exclusions whose reach no inode ruleset can carve out ahead of time.
    /// See [`DenyReach::compile`] for why they are reported rather than
    /// enforced.
    pub(super) unenforced_exclusions: Vec<String>,
}

/// Compile the workspace half of the ruleset for `profile`.
pub(super) fn workspace_read_grants(
    workspace_root: &Path,
    profile: &ResolvedFsProfile,
) -> Result<WorkspaceReadGrants, OrbitError> {
    let rules = profile.compile(FsOperation::Read)?;
    if rules.grants_nothing() {
        return Ok(WorkspaceReadGrants::default());
    }

    let reach = DenyReach::compile(workspace_root, &rules)?;
    let grants = if rules.allows(".")? {
        grant_read_tree(workspace_root, workspace_root, &rules, &reach)?
    } else {
        allowed_subtrees(workspace_root, workspace_root, &rules, &reach)?
    };

    Ok(WorkspaceReadGrants {
        grants,
        unenforced_exclusions: reach.unenforced,
    })
}

/// Where the profile's exclusions can still name a path that does not exist.
///
/// # Why a ruleset needs this
/// Landlock rules bind to inodes, and the ruleset is fixed before the child
/// starts. Granting a directory as a readable tree therefore grants every name
/// that will ever appear in it. Consulting only the denied paths present at
/// compile time makes the boundary depend on timing: the same profile enforces
/// a secret that already exists and discloses the identical secret written a
/// moment later.
///
/// Asking the rules instead removes the timing. A directory a bounded
/// exclusion can name into is granted list-only, so a name appearing there
/// afterwards has no readable ancestor, exactly as if it had been present all
/// along.
///
/// # What stays unenforced, and why it is reported
/// An exclusion whose reach is unbounded — a `**` that crosses directories, as
/// in `**/.env` — can name a path beneath *every* directory in the workspace.
/// Carving that out ahead of time means granting no directory as a tree at
/// all, which withdraws read access from every file the run itself produces:
/// a compiler cannot read back the object it just wrote, and the boundary
/// stops being one any build can run under (F2026-09-054 measured exactly
/// this). Such a rule is not silently dropped either. It is carried out on
/// [`WorkspaceReadGrants::unenforced_exclusions`] so the spawn boundary can
/// say which part of the profile the kernel is not holding, and it keeps its
/// full effect in the request-time policy check, which decides a concrete path
/// and needs no ruleset.
struct DenyReach<'a> {
    workspace_root: &'a Path,
    /// Exclusions whose reach their own segments bound.
    bounded: Vec<GlobReach>,
    /// Exclusions reported rather than compiled, as written.
    unenforced: Vec<String>,
}

impl<'a> DenyReach<'a> {
    fn compile(
        workspace_root: &'a Path,
        rules: &CompiledFsRules,
    ) -> Result<DenyReach<'a>, OrbitError> {
        let mut bounded = Vec::new();
        let mut unenforced = Vec::new();
        for exclusion in rules.exclusions() {
            let reach = GlobReach::compile(exclusion).map_err(|error| {
                OrbitError::InvalidInput(format!(
                    "landlock cannot compile read exclusion `{exclusion}`: {error}"
                ))
            })?;
            if reach.is_bounded() {
                bounded.push(reach);
            } else {
                unenforced.push(exclusion.to_string());
            }
        }
        Ok(DenyReach {
            workspace_root,
            bounded,
            unenforced,
        })
    }

    /// A reach that names nothing, for a carve-out driven by an explicit file
    /// list rather than by a rule set.
    fn none(workspace_root: &'a Path) -> DenyReach<'a> {
        DenyReach {
            workspace_root,
            bounded: Vec::new(),
            unenforced: Vec::new(),
        }
    }

    /// Whether a bounded exclusion can name a path beneath `dir`.
    ///
    /// A later positive rule may re-allow part of what an exclusion names.
    /// This does not model that: re-allowing narrows a grant rather than
    /// widening it, so treating the directory as reachable costs read access
    /// to names that do not exist yet and never discloses one.
    fn names_beneath(&self, dir: &Path) -> Result<bool, OrbitError> {
        if self.bounded.is_empty() {
            return Ok(false);
        }
        let relative = relative_to(self.workspace_root, dir)?;
        Ok(self
            .bounded
            .iter()
            .any(|reach| reach.names_beneath(&relative)))
    }
}

/// Grant `root` as a readable tree, carving out every path the profile denies
/// it — the ones present now, and the ones its exclusions can still name.
///
/// A directory holding a denied path cannot be granted as a tree, because a
/// path-beneath rule reaches every descendant. Such a directory is granted
/// list-only and each allowed child is granted in its own right, recursively.
/// The denied path is then left with no readable ancestor at all. A directory
/// a bounded exclusion can still name into is treated the same way, so the
/// grant does not depend on whether that name has been created yet.
///
/// # What this does and does not deny
/// Landlock rules bind to inodes, which decides the semantics at the edges.
/// Consequences, all covered by tests:
///
/// - A denied path stays unreadable for the child's whole life whether it
///   existed at spawn or appeared afterwards, including after a rename within
///   its directory: the inode keeps no readable ancestor.
/// - The child cannot move a denied file into a readable directory. Landlock
///   refuses a rename that would give a file more access at its destination,
///   which is why `REFER` is handled.
/// - The boundary governs acquisition, not naming. A file the ruleset already
///   grants keeps its grant through a rename or a hard link into a denied
///   name, because the grant is on the inode and the child could read those
///   bytes before it renamed anything. Bytes already in the child's memory, an
///   open descriptor, or a mapping are equally beyond recall.
/// - An exclusion whose reach is unbounded is reported rather than carved out;
///   see [`DenyReach`].
fn grant_read_tree(
    workspace_root: &Path,
    root: &Path,
    rules: &CompiledFsRules,
    reach: &DenyReach<'_>,
) -> Result<Vec<LandlockPathGrant>, OrbitError> {
    let denied = denied_paths(workspace_root, root, rules)?;
    carve_out_beneath(root, &denied, reach)
}

/// Walk into a workspace whose root is not itself allowed, granting the
/// allowed subtrees found along the way.
fn allowed_subtrees(
    workspace_root: &Path,
    dir: &Path,
    rules: &CompiledFsRules,
    reach: &DenyReach<'_>,
) -> Result<Vec<LandlockPathGrant>, OrbitError> {
    let mut grants = Vec::new();
    for child in children_under(workspace_root, dir)? {
        if rules.allows(&relative_to(workspace_root, &child)?)? {
            grants.extend(grant_read_tree(workspace_root, &child, rules, reach)?);
        } else if child.is_dir() {
            grants.extend(allowed_subtrees(workspace_root, &child, rules, reach)?);
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
/// ancestor. Used by the host grants, which carve credential files out of an
/// otherwise readable tool state directory from a fixed file list rather than
/// from a rule set.
pub(super) fn carve_out(
    root: &Path,
    denied: &BTreeSet<PathBuf>,
) -> Result<Vec<LandlockPathGrant>, OrbitError> {
    carve_out_beneath(root, denied, &DenyReach::none(root))
}

/// Grant `root` while leaving both the denied paths beneath it and the paths
/// `reach` can still name there without a readable ancestor.
fn carve_out_beneath(
    root: &Path,
    denied: &BTreeSet<PathBuf>,
    reach: &DenyReach<'_>,
) -> Result<Vec<LandlockPathGrant>, OrbitError> {
    if denied.contains(root) {
        return Ok(Vec::new());
    }
    let holds_denied_path = denied.iter().any(|path| path.starts_with(root));
    if !holds_denied_path && !reach.names_beneath(root)? {
        return Ok(vec![whole_path_grant(root)]);
    }
    if !root.is_dir() {
        return Ok(Vec::new());
    }

    let mut grants = vec![LandlockPathGrant::list_only(root.to_path_buf())];
    for child in children_under(root, root)? {
        grants.extend(carve_out_beneath(&child, denied, reach)?);
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
