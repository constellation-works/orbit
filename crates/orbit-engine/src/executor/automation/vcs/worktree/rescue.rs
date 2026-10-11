//! Where a settled run's uncommitted candidate survives once GC removes its
//! dirty worktree [ORB-15613].
//!
//! A failed claimed leaf keeps its uncommitted candidate in its worktree by
//! design, so the dirty-tree safety net used to retain the whole checkout —
//! build output included — after its task was done. The scheduled sweep now
//! removes such a worktree once the candidate is recoverable elsewhere, in
//! this order: a durable carry ref or the run's published branch on `origin`
//! whose tree is exactly the worktree's candidate, or a patch GC writes to a
//! bounded rescue store and verifies before removal. Anything GC cannot
//! capture exactly keeps the worktree.
//!
//! The candidate is what the carry stages: every tracked change and every
//! untracked path Git does not ignore, as one tree over `HEAD`. Ignored
//! output is rebuildable and is not part of it, as for a clean removal.

use std::fs;
use std::path::{Component, Path, PathBuf};

use orbit_common::OrbitError;
use serde::Serialize;

use super::super::git::{git_output, git_output_with_env, git_run_bytes};
use super::super::operations::CANDIDATE_REF_PREFIX;

/// The store's directory under the owning checkout.
const RESCUE_STORE: &str = ".orbit/state/rescue/worktree-gc";
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
    // never overwrites a verified patch with a different tree's.
    let short_tree = snapshot.tree.get(..12).unwrap_or(&snapshot.tree);
    let file = store.join(format!("{}-{short_tree}.patch", file_stem(run_id)));
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
         Tree: {}\n\nApply with `git apply` on a checkout of the base commit.\n\n",
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
