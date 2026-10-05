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
    carve_out_beneath(root, &denied, reach, &mut BTreeSet::new())
}

/// Walk into a workspace whose root is not itself allowed, granting the
/// allowed subtrees found along the way.
fn allowed_subtrees(
    workspace_root: &Path,
    dir: &Path,
    rules: &CompiledFsRules,
    reach: &DenyReach<'_>,
) -> Result<Vec<LandlockPathGrant>, OrbitError> {
    allowed_subtrees_in(workspace_root, dir, rules, reach, &mut BTreeSet::new())
}

fn allowed_subtrees_in(
    workspace_root: &Path,
    dir: &Path,
    rules: &CompiledFsRules,
    reach: &DenyReach<'_>,
    walked: &mut BTreeSet<PathBuf>,
) -> Result<Vec<LandlockPathGrant>, OrbitError> {
    // `dir` may be an alias of a directory this walk is already inside.
    if !enter_dir(walked, dir) {
        return Ok(Vec::new());
    }
    let mut grants = Vec::new();
    for child in children_under(workspace_root, dir)? {
        if rules.allows(&relative_to(workspace_root, &child)?)? {
            grants.extend(grant_read_tree(workspace_root, &child, rules, reach)?);
        } else if child.is_dir() {
            grants.extend(allowed_subtrees_in(
                workspace_root,
                &child,
                rules,
                reach,
                walked,
            )?);
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
        collect_denied(
            workspace_root,
            root,
            rules,
            &mut denied,
            &mut BTreeSet::new(),
        )?;
    }
    Ok(denied)
}

fn collect_denied(
    workspace_root: &Path,
    dir: &Path,
    rules: &CompiledFsRules,
    denied: &mut BTreeSet<PathBuf>,
    walked: &mut BTreeSet<PathBuf>,
) -> Result<(), OrbitError> {
    if !enter_dir(walked, dir) {
        return Ok(());
    }
    for child in children_under(workspace_root, dir)? {
        if !rules.allows(&relative_to(workspace_root, &child)?)? {
            denied.insert(child);
        } else if child.is_dir() {
            collect_denied(workspace_root, &child, rules, denied, walked)?;
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
    carve_out_beneath(root, denied, &DenyReach::none(root), &mut BTreeSet::new())
}

/// Grant `root` while leaving every denied path beneath it with no grant at
/// all — not even a listable ancestor.
///
/// [`carve_out`] leaves the directory holding a denied path granted list-only,
/// which is right for a credential *file* beside readable siblings: the
/// directory still has to be listable for the tool that owns it to work. A
/// denied *directory* needs more, because Landlock rights apply down the whole
/// hierarchy: a list-only ancestor would let the child enumerate the denied
/// directory's contents, which for a directory of credentials is most of the
/// disclosure. Here every allowed child is granted in its own right and no
/// ancestor of a denied path is granted anything, so listing it is refused
/// too. The cost is that the ancestors themselves stop being listable; naming
/// a file inside a denied directory as its own read root is how the one entry
/// a child is entitled to is granted back [ORB-12798].
pub(super) fn carve_out_unlistable(
    root: &Path,
    denied: &BTreeSet<PathBuf>,
) -> Result<Vec<LandlockPathGrant>, OrbitError> {
    carve_out_unlistable_in(root, denied, &mut BTreeSet::new())
}

fn carve_out_unlistable_in(
    root: &Path,
    denied: &BTreeSet<PathBuf>,
    walked: &mut BTreeSet<PathBuf>,
) -> Result<Vec<LandlockPathGrant>, OrbitError> {
    if denied.contains(root) {
        return Ok(Vec::new());
    }
    if !denied.iter().any(|path| path.starts_with(root)) {
        return Ok(vec![whole_path_grant(root)]);
    }
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    if !enter_dir(walked, root) {
        return Ok(Vec::new());
    }
    let mut grants = Vec::new();
    for child in children_under(root, root)? {
        grants.extend(carve_out_unlistable_in(&child, denied, walked)?);
    }
    Ok(grants)
}

/// Grant `root` for an explicit boundary: every `unlistable` path keeps no
/// granted ancestor ([`carve_out_unlistable`]), and every `listable` path
/// keeps a list-only one ([`carve_out`]).
///
/// The two answer different owners. `unlistable` holds host trees — callback
/// sessions, grant witnesses, other plugins' state — whose names are part of
/// what is protected. `listable` holds an agent's own read exclusions, which
/// the activity ruleset carves with list-only ancestors, so a brokered backend
/// sees the workspace the way the agent would. With no `listable` path this
/// is exactly [`carve_out_unlistable`].
pub(super) fn carve_out_boundary(
    root: &Path,
    unlistable: &BTreeSet<PathBuf>,
    listable: &BTreeSet<PathBuf>,
) -> Result<Vec<LandlockPathGrant>, OrbitError> {
    carve_out_boundary_in(root, unlistable, listable, &mut BTreeSet::new())
}

fn carve_out_boundary_in(
    root: &Path,
    unlistable: &BTreeSet<PathBuf>,
    listable: &BTreeSet<PathBuf>,
    walked: &mut BTreeSet<PathBuf>,
) -> Result<Vec<LandlockPathGrant>, OrbitError> {
    if listable.is_empty() {
        return carve_out_unlistable(root, unlistable);
    }
    if unlistable.contains(root) || listable.contains(root) {
        return Ok(Vec::new());
    }
    if !unlistable.iter().any(|path| path.starts_with(root)) {
        return carve_out(root, listable);
    }
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    if !enter_dir(walked, root) {
        return Ok(Vec::new());
    }
    let mut grants = Vec::new();
    for child in children_under(root, root)? {
        grants.extend(carve_out_boundary_in(&child, unlistable, listable, walked)?);
    }
    Ok(grants)
}

/// Grant `root` writable without a readable ancestor over any path in
/// `unlistable` or `listable`.
///
/// A read-write directory grant includes read rights and applies to
/// everything beneath the directory, so a write root at or above a read deny
/// would undo the read carve. That directory is granted write rights only.
/// Each child that does not meet a restriction is granted read-write in its
/// own right. A
/// restriction that does not exist yet still removes read from its
/// ancestors, so a name created there after spawn is not readable either.
/// Write rights cannot be taken back from a descendant once an ancestor has
/// them: the excluded subtree stays writable.
pub(super) fn carve_out_write(
    root: &Path,
    unlistable: &BTreeSet<PathBuf>,
    listable: &BTreeSet<PathBuf>,
) -> Result<Vec<LandlockPathGrant>, OrbitError> {
    // A write root nested inside a broader deny, with no restriction beneath
    // it, does not expose the rest of that deny. The plugin's own state
    // directory is that case: `state/plugins` is unreadable, and
    // `state/plugins/<ns>` stays read-write.
    if nested_write_island(root, unlistable, listable) {
        return Ok(vec![whole_write_grant(root)]);
    }
    carve_out_write_in(root, unlistable, listable, &mut BTreeSet::new())
}

/// Whether compiling `path` has to withhold read because a restriction is at
/// or beneath it. A write root merely nested inside a broader deny does not.
pub(super) fn write_read_is_carved(
    path: &Path,
    unlistable: &BTreeSet<PathBuf>,
    listable: &BTreeSet<PathBuf>,
) -> bool {
    overlaps_read_restriction(path, unlistable, listable)
        && !nested_write_island(path, unlistable, listable)
}

/// `path` is strictly inside some restriction and is not itself at or above one.
fn nested_write_island(
    path: &Path,
    unlistable: &BTreeSet<PathBuf>,
    listable: &BTreeSet<PathBuf>,
) -> bool {
    let inside = strictly_inside(path, unlistable) || strictly_inside(path, listable);
    let covers = covers_restriction(path, unlistable) || covers_restriction(path, listable);
    inside && !covers
}

fn strictly_inside(path: &Path, restrictions: &BTreeSet<PathBuf>) -> bool {
    restrictions
        .iter()
        .any(|restriction| path != restriction && path.starts_with(restriction))
}

fn covers_restriction(path: &Path, restrictions: &BTreeSet<PathBuf>) -> bool {
    restrictions
        .iter()
        .any(|restriction| restriction == path || restriction.starts_with(path))
}

fn carve_out_write_in(
    root: &Path,
    unlistable: &BTreeSet<PathBuf>,
    listable: &BTreeSet<PathBuf>,
    walked: &mut BTreeSet<PathBuf>,
) -> Result<Vec<LandlockPathGrant>, OrbitError> {
    if !overlaps_read_restriction(root, unlistable, listable) {
        return Ok(vec![whole_write_grant(root)]);
    }
    // Inside a restriction, or a file that cannot hold a descendant: one
    // write-only rule. Walking further would only risk a read-write grant
    // on something the restriction already covers.
    if contained_in_read_restriction(root, unlistable, listable) || !root.is_dir() {
        return Ok(vec![write_only_grant(root)]);
    }
    if !enter_dir(walked, root) {
        return Ok(Vec::new());
    }
    let mut grants = vec![LandlockPathGrant::write_only_tree(root.to_path_buf())];
    for child in children_under(root, root)? {
        grants.extend(carve_out_write_in(&child, unlistable, listable, walked)?);
    }
    Ok(grants)
}

/// Whether granting read on `path` would expose a restriction, or `path` is
/// itself inside one.
pub(super) fn overlaps_read_restriction(
    path: &Path,
    unlistable: &BTreeSet<PathBuf>,
    listable: &BTreeSet<PathBuf>,
) -> bool {
    restriction_meets(path, unlistable) || restriction_meets(path, listable)
}

fn contained_in_read_restriction(
    path: &Path,
    unlistable: &BTreeSet<PathBuf>,
    listable: &BTreeSet<PathBuf>,
) -> bool {
    restriction_contains(path, unlistable) || restriction_contains(path, listable)
}

fn restriction_meets(path: &Path, restrictions: &BTreeSet<PathBuf>) -> bool {
    restrictions
        .iter()
        .any(|restriction| path.starts_with(restriction) || restriction.starts_with(path))
}

fn restriction_contains(path: &Path, restrictions: &BTreeSet<PathBuf>) -> bool {
    restrictions
        .iter()
        .any(|restriction| path == restriction || path.starts_with(restriction))
}

fn whole_write_grant(path: &Path) -> LandlockPathGrant {
    if path.is_dir() {
        LandlockPathGrant::write_tree(path.to_path_buf())
    } else {
        LandlockPathGrant::write_file(path.to_path_buf())
    }
}

fn write_only_grant(path: &Path) -> LandlockPathGrant {
    if path.is_dir() {
        LandlockPathGrant::write_only_tree(path.to_path_buf())
    } else {
        LandlockPathGrant::write_only_file(path.to_path_buf())
    }
}

/// Grant `root` while leaving both the denied paths beneath it and the paths
/// `reach` can still name there without a readable ancestor.
fn carve_out_beneath(
    root: &Path,
    denied: &BTreeSet<PathBuf>,
    reach: &DenyReach<'_>,
    walked: &mut BTreeSet<PathBuf>,
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
    if !enter_dir(walked, root) {
        return Ok(Vec::new());
    }

    let mut grants = vec![LandlockPathGrant::list_only(root.to_path_buf())];
    for child in children_under(root, root)? {
        grants.extend(carve_out_beneath(&child, denied, reach, walked)?);
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
/// A directory symlink is returned as its canonical target. That target may
/// be `dir` itself (`loop -> .`) or another directory this walk has already
/// entered, and the canonical path does not grow, so the kernel `ELOOP`
/// limit never stops the walk. Recursive callers bound that with
/// [`enter_dir`]: the first visit grants the target, and a later alias of
/// the same directory is not entered again.
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

/// Whether `dir` is new to this walk.
///
/// Keys are the canonical paths [`children_under`] already returns. A second
/// sighting is an alias (or a cycle back to one), not a new tree: its
/// children were listed on the first visit, which is what keeps an in-bound
/// alias's grants while a cycle terminates.
fn enter_dir(walked: &mut BTreeSet<PathBuf>, dir: &Path) -> bool {
    walked.insert(dir.to_path_buf())
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
