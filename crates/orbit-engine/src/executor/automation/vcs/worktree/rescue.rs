//! Where a settled run's uncommitted candidate survives once GC removes its
//! dirty worktree [ORB-15613].
//!
//! A failed claimed leaf keeps its uncommitted candidate in its worktree by
//! design, so the dirty-tree safety net used to retain the whole checkout —
//! build output included — after its task was done. The scheduled sweep now
//! removes such a worktree once the candidate is recoverable elsewhere, in
//! this order: a durable carry ref or the run's published branch on `origin`
//! whose tree is exactly the worktree's candidate, or a patch GC writes to a
//! bounded rescue store and verifies before removal. The patch applies to
//! the worktree's `HEAD`. GC pins that commit on
//! `refs/orbit/rescue/worktree-gc/<run>-<tree>` in the owning repository
//! before it deletes the run branch, so an unpublished `HEAD` stays
//! reachable after Git drops the branch and expires reflogs, including
//! files that commit introduced and the diff does not repeat. A deleting
//! sweep removes the pin once the patch file is gone. Anything GC cannot
//! capture exactly keeps the worktree.
//!
//! The candidate is what the carry stages: every tracked change and every
//! untracked path Git does not ignore, as one tree over `HEAD`. Ignored
//! output is rebuildable and is not part of it, as for a clean removal.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use orbit_common::OrbitError;
use serde::Serialize;

use super::super::git::{git_output, git_output_with_env, git_run_bytes, git_success};
use super::super::operations::CANDIDATE_REF_PREFIX;

/// The store's directory under the owning checkout.
const RESCUE_STORE: &str = ".orbit/state/rescue/worktree-gc";
/// Local ref that keeps a rescue patch's base commit reachable after GC
/// deletes the run branch. One ref per patch file, same leaf name.
const RESCUE_BASE_REF_PREFIX: &str = "refs/orbit/rescue/worktree-gc/";
/// Largest patch GC writes; a bigger candidate keeps its worktree.
const MAX_RESCUE_PATCH_BYTES: u64 = 16 * 1024 * 1024;
/// Total the store may hold; when full, GC keeps worktrees instead.
const MAX_RESCUE_STORE_BYTES: u64 = 256 * 1024 * 1024;
/// Scratch index name under the worktree's private Git directory.
const SCRATCH_INDEX: &str = "orbit-gc-rescue.index";

/// Where a removed worktree's uncommitted candidate can be recovered.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorktreeGcRecovery {
    /// `carry_ref`, `published_branch` or `rescue_patch`.
    pub source: String,
    /// The ref on `origin`, or the patch file.
    pub location: String,
    /// The ref's commit, or the commit the patch applies to.
    pub commit: String,
}

/// The worktree's candidate: `HEAD` plus every uncommitted change Git does
/// not ignore, as one tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CandidateSnapshot {
    pub(super) head: String,
    pub(super) tree: String,
}

/// Snapshot the candidate through a scratch index, leaving the worktree's own
/// index and files untouched. The error is why GC cannot vouch for a
/// snapshot: content a tree cannot carry (a nested repository, submodule
/// changes) keeps the worktree.
pub(super) fn candidate_snapshot(worktree: &Path) -> Result<CandidateSnapshot, String> {
    let snapshot = (|| -> Result<CandidateSnapshot, OrbitError> {
        let head = git_output(worktree, &["rev-parse", "--verify", "HEAD^{commit}"])?;
        let head_tree = git_output(worktree, &["rev-parse", "--verify", "HEAD^{tree}"])?;
        let tree = with_scratch_index(worktree, |scratch| {
            git_output_with_env(worktree, &["read-tree", "HEAD"], scratch)?;
            git_output_with_env(worktree, &["add", "--all", "--", "."], scratch)?;
            git_output_with_env(worktree, &["write-tree"], scratch)
        })?;
        // Only paths changed in the real index need this check: an ordinary
        // unstaged edit differs from the index but has no unique staged state.
        // A single-tree recovery cannot preserve two versions of a staged
        // path, even when another working edit makes the snapshot nonempty.
        let staged = index_changed_paths(worktree, &head)?;
        if !staged.is_empty() && !staged.is_disjoint(&index_changed_paths(worktree, &tree)?) {
            return Err(OrbitError::Execution(
                "staged paths differ from the working candidate; a single-tree recovery cannot preserve both versions"
                    .to_string(),
            ));
        }
        if tree == head_tree {
            return Err(OrbitError::Execution(
                "Git reports changes no tree snapshot captures (submodule content or index-only state)"
                    .to_string(),
            ));
        }
        let gitlinks = git_output(
            worktree,
            &["diff-tree", "-r", "--no-renames", "--raw", &head, &tree],
        )?;
        if gitlinks
            .lines()
            .any(|line| line.split(' ').nth(1) == Some("160000"))
        {
            return Err(OrbitError::Execution(
                "the candidate holds a nested repository whose history a snapshot cannot carry"
                    .to_string(),
            ));
        }
        Ok(CandidateSnapshot { head, tree })
    })();
    snapshot.map_err(|error| format!("candidate snapshot refused: {error}"))
}

/// Paths whose real index entries differ from a tree. NUL-delimited bytes
/// preserve arbitrary Git path names, and disabling renames checks both ends.
fn index_changed_paths(worktree: &Path, tree: &str) -> Result<BTreeSet<Vec<u8>>, OrbitError> {
    let diff = git_run_bytes(
        worktree,
        &[
            "diff",
            "--cached",
            "--name-only",
            "-z",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            tree,
            "--",
        ],
        None,
    )?;
    if !diff.success || diff.timed_out {
        return Err(OrbitError::Execution(format!(
            "cannot compare staged paths: git diff exited {:?}: {}",
            diff.exit_code,
            diff.stderr.trim()
        )));
    }
    Ok(diff
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(<[u8]>::to_vec)
        .collect())
}

/// A ref on `origin` that already holds exactly this candidate: the run's
/// durable carry ref, then its published branch. A ref whose commit is not in
/// this object store cannot be compared and does not count; neither does an
/// unreachable `origin`.
pub(super) fn published_recovery(
    worktree: &Path,
    branch: &str,
    task_ids: &[String],
    run_id: &str,
    snapshot: &CandidateSnapshot,
) -> Option<WorktreeGcRecovery> {
    let carry_refs = task_ids
        .iter()
        .map(|task_id| format!("{CANDIDATE_REF_PREFIX}{task_id}/{run_id}"))
        .collect::<Vec<_>>();
    let branch_ref = format!("refs/heads/{branch}");
    let mut args = vec!["ls-remote", "origin"];
    args.extend(carry_refs.iter().map(String::as_str));
    args.push(branch_ref.as_str());
    let listed = match git_output(worktree, &args) {
        Ok(listed) => listed,
        Err(error) => {
            tracing::info!(path = %worktree.display(), %error,
                "worktree GC could not list origin's candidate refs; falling back to a rescue patch");
            return None;
        }
    };
    let remote = listed
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(sha, name)| (name.trim().to_string(), sha.trim().to_string()))
        .collect::<Vec<_>>();
    carry_refs
        .iter()
        .map(|name| (name, "carry_ref"))
        .chain(std::iter::once((&branch_ref, "published_branch")))
        .find_map(|(name, source)| {
            let (_, sha) = remote.iter().find(|(listed, _)| listed == name)?;
            let tree = git_output(
                worktree,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{sha}^{{tree}}"),
                ],
            )
            .ok()?;
            (tree == snapshot.tree).then(|| WorktreeGcRecovery {
                source: source.to_string(),
                location: name.clone(),
                commit: sha.clone(),
            })
        })
}

/// Write the candidate as a patch against `snapshot.head` to the bounded
/// rescue store, then prove it rebuilds the snapshot's exact tree. A dry run
/// checks the bounds and names the file without writing it. The error is why
/// the candidate keeps its worktree.
pub(super) fn write_rescue_patch(
    repo_root: &Path,
    worktree: &Path,
    run_id: &str,
    branch: &str,
    snapshot: &CandidateSnapshot,
    write: bool,
) -> Result<WorktreeGcRecovery, String> {
    let store = validated_rescue_store(repo_root).ok_or_else(|| {
        format!(
            "rescue store refused: checkout path '{}' is not absolute and normalized",
            repo_root.display()
        )
    })?;
    ensure_real_store_directories(&store)?;
    // Named by the candidate's tree too, so a later sweep of the same run
    // never overwrites a verified patch with a different tree's. The same
    // leaf names the base ref, so the patch and the commit it applies to
    // stay paired.
    let leaf = rescue_patch_leaf(run_id, &snapshot.tree)?;
    let base_ref = format!("{RESCUE_BASE_REF_PREFIX}{leaf}");
    let file = store.join(format!("{leaf}.patch"));
    let diff = git_run_bytes(
        worktree,
        &[
            "diff",
            "--binary",
            "--full-index",
            "--no-renames",
            "--no-color",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--no-ext-diff",
            "--no-textconv",
            &snapshot.head,
            &snapshot.tree,
        ],
        None,
    )
    .map_err(|error| format!("rescue patch failed: {error}"))?;
    if !diff.success || diff.timed_out {
        return Err(format!(
            "rescue patch failed: git diff exited {:?}: {}",
            diff.exit_code,
            diff.stderr.trim()
        ));
    }
    let mut patch = format!(
        "Orbit worktree GC rescue patch\n\nRun: {run_id}\nBranch: {branch}\nBase: {}\n\
         Base-Ref: {base_ref}\nTree: {}\n\nApply with `git apply` on a checkout of \
         the base commit. Base-Ref keeps that commit reachable after the run branch \
         is deleted.\n\n",
        snapshot.head, snapshot.tree
    )
    .into_bytes();
    patch.extend_from_slice(&diff.stdout);
    let size = patch.len() as u64;
    if size > MAX_RESCUE_PATCH_BYTES {
        return Err(format!(
            "rescue patch of {size} bytes exceeds the {MAX_RESCUE_PATCH_BYTES}-byte limit"
        ));
    }
    let held = store_bytes(&store, &file);
    if held.saturating_add(size) > MAX_RESCUE_STORE_BYTES {
        return Err(format!(
            "rescue store '{}' holds {held} bytes; a {size}-byte patch would pass its \
             {MAX_RESCUE_STORE_BYTES}-byte limit. Apply or delete old patches there",
            store.display()
        ));
    }
    let recovery = WorktreeGcRecovery {
        source: "rescue_patch".to_string(),
        location: file.display().to_string(),
        commit: snapshot.head.clone(),
    };
    if !write {
        return Ok(recovery);
    }
    // Staged and renamed, so a partial write never stands in for a patch.
    orbit_common::fs::io::atomic_write_bytes(&file, &patch).map_err(|error| {
        format!(
            "rescue patch could not be written to '{}': {error}",
            file.display()
        )
    })?;
    if let Err(error) = verify_patch(worktree, &file, snapshot) {
        if let Err(remove) = fs::remove_file(&file) {
            tracing::warn!(path = %file.display(), %remove, "unverified rescue patch left behind");
        }
        return Err(format!("rescue patch did not verify: {error}"));
    }
    // The caller deletes the run branch next. When that branch is the only
    // root for an unpublished HEAD, the SHA in the patch text does not keep
    // the commit — or files it introduced that this diff does not repeat.
    if let Err(error) = retain_patch_base(repo_root, &base_ref, &snapshot.head) {
        if let Err(remove) = fs::remove_file(&file) {
            tracing::warn!(path = %file.display(), %remove, "unpinned rescue patch left behind");
        }
        return Err(format!("rescue patch base was not retained: {error}"));
    }
    Ok(recovery)
}

/// Rebuild the candidate's tree from `HEAD` and the patch file alone.
fn verify_patch(
    worktree: &Path,
    file: &Path,
    snapshot: &CandidateSnapshot,
) -> Result<(), OrbitError> {
    let file = file.to_string_lossy().into_owned();
    let tree = with_scratch_index(worktree, |scratch| {
        git_output_with_env(worktree, &["read-tree", &snapshot.head], scratch)?;
        git_output_with_env(worktree, &["apply", "--cached", "--binary", &file], scratch)?;
        git_output_with_env(worktree, &["write-tree"], scratch)
    })?;
    if tree == snapshot.tree {
        Ok(())
    } else {
        Err(OrbitError::Execution(format!(
            "it rebuilds tree {tree}, not the candidate's {}",
            snapshot.tree
        )))
    }
}

/// The rescue store under an absolute checkout path with no parent
/// components; the CodeQL path model treats this as its validation barrier.
fn validated_rescue_store(repo_root: &Path) -> Option<PathBuf> {
    (repo_root.is_absolute()
        && repo_root
            .components()
            .all(|component| !matches!(component, Component::ParentDir)))
    .then(|| repo_root.join(RESCUE_STORE))
}

/// Every existing store component below the checkout must be a real
/// directory, so an agent-written checkout cannot redirect GC's writes and
/// deletions through a symlink.
fn ensure_real_store_directories(store: &Path) -> Result<(), String> {
    let depth = Path::new(RESCUE_STORE).components().count();
    let mut directories = store.ancestors().take(depth).collect::<Vec<_>>();
    directories.reverse();
    for directory in directories {
        match fs::symlink_metadata(directory) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Ok(_) => {
                return Err(format!(
                    "rescue store refused: '{}' is not a real directory",
                    directory.display()
                ));
            }
            Err(error) => {
                return Err(format!(
                    "rescue store refused: cannot inspect '{}': {error}",
                    directory.display()
                ));
            }
        }
    }
    Ok(())
}

/// Bytes the store holds besides `replaced`, which a rewrite supersedes.
fn store_bytes(store: &Path, replaced: &Path) -> u64 {
    fs::read_dir(store)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path() != replaced)
        .filter_map(|entry| entry.metadata().ok())
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len())
        .sum()
}

/// A file name for a run id: anything outside `[A-Za-z0-9._-]` becomes `_`.
fn file_stem(run_id: &str) -> String {
    run_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The patch file's leaf and the last component of its base ref.
fn rescue_patch_leaf(run_id: &str, tree: &str) -> Result<String, String> {
    let short_tree = tree.get(..12).unwrap_or(tree);
    let leaf = format!("{}-{short_tree}", file_stem(run_id));
    let name = format!("{RESCUE_BASE_REF_PREFIX}{leaf}");
    if valid_rescue_base_ref(&name) {
        Ok(leaf)
    } else {
        Err(format!("rescue base ref '{name}' is not a plain ref"))
    }
}

/// One plain path segment under the rescue base prefix, so the name can be
/// neither an option nor a path that escapes the rescue namespace.
fn valid_rescue_base_ref(value: &str) -> bool {
    value
        .strip_prefix(RESCUE_BASE_REF_PREFIX)
        .is_some_and(|rest| {
            !rest.is_empty()
                && !rest.contains('/')
                && !rest.starts_with('.')
                && !rest.ends_with(".lock")
                && !rest.contains("..")
                && rest
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
}

/// Point `base_ref` at `commit` in the owning repository. The worktree and
/// the run branch are about to disappear; a commit that exists only in the
/// worktree's private objects cannot be kept by a ref here.
fn retain_patch_base(repo_root: &Path, base_ref: &str, commit: &str) -> Result<(), String> {
    let found = git_output(
        repo_root,
        &["rev-parse", "--verify", &format!("{commit}^{{commit}}")],
    )
    .map_err(|error| {
        format!("the owning repository does not contain the patch base {commit}: {error}")
    })?;
    if found != commit {
        return Err(format!(
            "the owning repository resolved the patch base to {found}, not {commit}"
        ));
    }
    git_success(repo_root, &["update-ref", "--no-deref", base_ref, commit])
        .map_err(|error| format!("could not point {base_ref} at {commit}: {error}"))?;
    match git_output(
        repo_root,
        &["rev-parse", "--verify", &format!("{base_ref}^{{commit}}")],
    ) {
        Ok(pinned) if pinned == commit => Ok(()),
        Ok(pinned) => {
            if let Err(error) = git_success(repo_root, &["update-ref", "-d", base_ref]) {
                tracing::warn!(%base_ref, %error, "rescue base ref left pointing at the wrong commit");
            }
            Err(format!(
                "{base_ref} points at {pinned}, not the patch base {commit}"
            ))
        }
        Err(error) => Err(format!(
            "could not read {base_ref} after pinning it: {error}"
        )),
    }
}

/// Drop base refs whose patch file is gone. A symlinked store is not
/// evidence the patches were removed, so those refs stay. An unreadable or
/// unexpected file at the patch path also keeps its ref.
pub(super) fn release_orphan_rescue_bases(repo_root: &Path) -> Result<(), OrbitError> {
    let Some(store) = validated_rescue_store(repo_root) else {
        return Ok(());
    };
    if !rescue_store_allows_release(&store) {
        tracing::warn!(
            store = %store.display(),
            "rescue base refs kept; the rescue store is not a real directory"
        );
        return Ok(());
    }
    let listed = git_output(
        repo_root,
        &[
            "for-each-ref",
            "--format=%(refname)",
            RESCUE_BASE_REF_PREFIX,
        ],
    )?;
    for name in listed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if !valid_rescue_base_ref(name) {
            continue;
        }
        let Some(leaf) = name.strip_prefix(RESCUE_BASE_REF_PREFIX) else {
            continue;
        };
        if !rescue_patch_is_gone(&store.join(format!("{leaf}.patch"))) {
            continue;
        }
        if let Err(error) = git_success(repo_root, &["update-ref", "-d", name]) {
            tracing::warn!(%name, %error, "rescue base ref could not be released");
        }
    }
    Ok(())
}

/// Whether the store's own components are real directories or simply absent.
/// A symlink or other non-directory stops the walk: release must not decide
/// from a redirected path.
fn rescue_store_allows_release(store: &Path) -> bool {
    let depth = Path::new(RESCUE_STORE).components().count();
    let mut directories = store.ancestors().take(depth).collect::<Vec<_>>();
    directories.reverse();
    for directory in directories {
        match fs::symlink_metadata(directory) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
            _ => return false,
        }
    }
    true
}

/// `true` only when the patch file is absent. Anything else — a real file, a
/// symlink, a directory, or an unreadable path — keeps the base ref.
fn rescue_patch_is_gone(patch: &Path) -> bool {
    matches!(
        fs::symlink_metadata(patch),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    )
}

/// Run `body` against a fresh scratch index, removing it afterwards.
fn with_scratch_index<T>(
    worktree: &Path,
    body: impl FnOnce(&[(&str, &str)]) -> Result<T, OrbitError>,
) -> Result<T, OrbitError> {
    let index = scratch_index(worktree)?;
    let index_env = index.to_string_lossy().into_owned();
    let result = body(&[("GIT_INDEX_FILE", index_env.as_str())]);
    if let Err(error) = fs::remove_file(&index)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(index = %index.display(), %error, "scratch index left behind");
    }
    result
}

/// Absolute: `--git-path` answers relative to the worktree when its Git
/// directory is inside it.
fn scratch_index(worktree: &Path) -> Result<PathBuf, OrbitError> {
    let path = PathBuf::from(git_output(
        worktree,
        &["rev-parse", "--git-path", SCRATCH_INDEX],
    )?);
    Ok(if path.is_absolute() {
        path
    } else {
        worktree.join(path)
    })
}
