//! Invocation-owned source checkouts for read-only inspection [ORB-11256].
//!
//! Slots are private scratch space in the source repository's Orbit state
//! directory, beside the managed worktrees. A slot holds ordinary tracked
//! content, which legitimately includes symlinks, so it stays outside the Git
//! metadata tree — host protection refuses every symlink and hard link there
//! [ORB-11756].
//!
//! A kernel lease lasts through provider exit and RAII cleanup. After a crash
//! the next holder removes the abandoned checkout before reuse. The fixed slot
//! count bounds leftovers even when no retry follows a crash. These are
//! standalone repositories: no shared index, alternates, registered worktree,
//! branch, or global worktree-prune operation is involved.
//!
//! A bad pin, profile, slot owner, missing commit, or a checkout that changed
//! during the read-only run is a permanent CLI failure: repeating the provider
//! call fails the same way. A leased-out pool and git or filesystem IO stay
//! retryable.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use orbit_common::fs::git::{
    GIT_BULK_COPY_TIMEOUT, GIT_CHECKOUT_TIMEOUT, GIT_LOCAL_TIMEOUT, GitCommandOutput, run_git,
    run_git_within,
};
use orbit_common::fs::io::atomic_write_text;
use serde_json::Value;

use super::super::dispatcher::DispatchError;

const SLOT_COUNT: usize = 16;
const POOL_DIR: &str = "source-inspections-v1";
const LEGACY_POOL_DIR: &str = "orbit-source-inspections-v1";
const OWNER: &str = "orbit-source-inspection-v1\n";

pub(super) struct SourceInspection {
    root: PathBuf,
    revision: String,
    // Drop removes the checkout before the file is closed and releases its lock.
    _lease: File,
}

impl SourceInspection {
    pub(super) fn from_input(
        input: &Value,
        source: Option<&Path>,
        fs_profile: Option<&str>,
    ) -> Result<Option<Self>, DispatchError> {
        let Some(value) = input.get("inspection_revision") else {
            return Ok(None);
        };
        if value.is_null() || value.as_str() == Some("") {
            if let Some(source) = source {
                let output = run_git(source, &["rev-parse", "--is-inside-work-tree"])
                    .map_err(|error| retryable(error.to_string()))?;
                if output.success && output.stdout.trim() == "true" {
                    return Err(permanent(
                        "a Git workspace requires a pinned inspection_revision",
                    ));
                }
            }
            return Ok(None);
        }
        let revision = value
            .as_str()
            .ok_or_else(|| permanent("inspection_revision must be a commit id"))?;
        if !matches!(revision.len(), 40 | 64) || !revision.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(permanent("inspection_revision must be a full commit id"));
        }
        if let Some(source_revision) = input.get("source_revision").and_then(Value::as_str)
            && source_revision != revision
        {
            return Err(permanent(
                "inspection_revision differs from the prepared source_revision",
            ));
        }
        if fs_profile != Some("reviewer") {
            return Err(permanent(
                "source inspection requires the reviewer filesystem profile",
            ));
        }
        let source = source.ok_or_else(|| permanent("source inspection requires a workspace"))?;
        Self::create(source, revision, GIT_BULK_COPY_TIMEOUT).map(Some)
    }

    /// Lease a slot and materialize `revision` in it. `copy_deadline` bounds
    /// the full-ancestry fetch; a copy that overruns it fails the lease and
    /// drops the half-built checkout, leaving the slot free for the next lease.
    pub(super) fn create(
        source: &Path,
        revision: &str,
        copy_deadline: Duration,
    ) -> Result<Self, DispatchError> {
        require_commit(source, revision)?;
        let common = git(
            source,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?;
        let common = common.trim();
        let format = git(source, &["rev-parse", "--show-object-format"])?;
        if let Err(error) = retire_legacy_pool(Path::new(common)) {
            tracing::warn!(%error, "retiring the in-metadata inspection pool needs operator attention");
        }
        let pool = prepare_pool(Path::new(common))?;
        for index in 0..SLOT_COUNT {
            let slot = pool.join(index.to_string());
            directory(&slot).map_err(io_failure)?;
            let lock_path = slot.join("lease");
            reject_symlink(&lock_path).map_err(io_failure)?;
            let lease = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&lock_path)
                .map_err(io_failure)?;
            match lease.try_lock() {
                Ok(()) => {}
                Err(TryLockError::WouldBlock) => continue,
                Err(TryLockError::Error(error)) => return Err(io_failure(error)),
            }
            let root = slot.join("checkout");
            let marker = slot.join("owner");
            reject_symlink(&marker).map_err(io_failure)?;
            if marker.exists() {
                if fs::read_to_string(&marker).map_err(io_failure)? != OWNER {
                    return Err(permanent("inspection slot has an unrecognized owner"));
                }
                remove_checkout(&root).map_err(io_failure)?;
            } else {
                if root.symlink_metadata().is_ok() {
                    return Err(permanent(
                        "refusing to remove an unowned inspection checkout",
                    ));
                }
                // Atomic: a crash mid-write must not leave an empty marker that
                // every later lease would reject as an unrecognized owner.
                atomic_write_text(&marker, OWNER).map_err(io_failure)?;
            }
            let inspection = Self {
                root,
                revision: revision.to_string(),
                _lease: lease,
            };
            directory(&inspection.root).map_err(io_failure)?;
            git(
                &inspection.root,
                &[
                    "init",
                    "--quiet",
                    "--template=",
                    &format!("--object-format={}", format.trim()),
                ],
            )?;
            // Fetch this revision with its full ancestry into a private object
            // set: the task-pilot contract inspects history from this checkout
            // (merge-base, log, show at an older commit), so the slot needs
            // more than the pinned commit itself. The fetch still copies
            // objects into this repository's own store rather than linking
            // alternates, so sandboxed Git stays independent from the primary
            // object database. The copy is bulk work, so it runs under its
            // own deadline rather than the local one.
            git_within(
                &inspection.root,
                copy_deadline,
                &[
                    "-c",
                    "protocol.file.allow=always",
                    "fetch",
                    "--quiet",
                    "--no-tags",
                    common,
                    revision,
                ],
            )?;
            git_within(
                &inspection.root,
                GIT_CHECKOUT_TIMEOUT,
                &["checkout", "--quiet", "--detach", revision],
            )?;
            inspection.verify()?;
            return Ok(inspection);
        }
        Err(retryable(
            "all source inspection slots are leased; retry after a pilot finishes",
        ))
    }

    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    pub(super) fn bind_input(&self, input: &Value) -> Value {
        let mut bound = input.clone();
        bound["workspace_path"] = self.root.display().to_string().into();
        bound["repo_root"] = self.root.display().to_string().into();
        bound["source_revision"] = self.revision.clone().into();
        bound
    }

    pub(super) fn verify(&self) -> Result<(), DispatchError> {
        if git(&self.root, &["rev-parse", "HEAD"])?.trim() != self.revision
            || !git(
                &self.root,
                &["status", "--porcelain=v1", "--untracked-files=all"],
            )?
            .trim()
            .is_empty()
        {
            return Err(permanent(
                "source inspection checkout changed during read-only execution",
            ));
        }
        Ok(())
    }
}

impl Drop for SourceInspection {
    fn drop(&mut self) {
        if let Err(error) = remove_checkout(&self.root) {
            tracing::warn!(path = %self.root.display(), %error, "inspection cleanup deferred to next lease holder");
        }
    }
}

/// Whether `checkout` is a slot checkout materialized for the repository whose
/// common Git directory is `common`.
///
/// A slot is a standalone repository, so it never shares `common` the way a
/// linked worktree does. Registered tools a provider invokes from the slot
/// recognize it here instead, so they run in the pinned checkout rather than
/// in the primary [ORB-13800]. Recognition requires the slot path, its owner
/// marker, a private `.git` directory and a detached HEAD whose commit exists
/// in `common`; a repository planted anywhere else, or a foreign one in a
/// slot, does not qualify.
pub fn is_source_inspection_checkout(common: &Path, checkout: &Path) -> bool {
    let Some(pool) = common
        .parent()
        .map(|repo_root| repo_root.join(".orbit/state").join(POOL_DIR))
        .and_then(|pool| pool.canonicalize().ok())
    else {
        return false;
    };
    let Ok(checkout) = checkout.canonicalize() else {
        return false;
    };
    if checkout.file_name() != Some("checkout".as_ref()) {
        return false;
    }
    let Some(slot) = checkout
        .parent()
        .filter(|slot| slot.parent() == Some(pool.as_path()))
    else {
        return false;
    };
    let in_pool = slot
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.parse::<usize>().ok())
        .is_some_and(|index| index < SLOT_COUNT);
    let git_dir = checkout.join(".git");
    let owned = in_pool
        && fs::symlink_metadata(&git_dir).is_ok_and(|metadata| metadata.is_dir())
        && fs::symlink_metadata(slot.join("owner")).is_ok_and(|metadata| metadata.is_file())
        && fs::read_to_string(slot.join("owner")).is_ok_and(|owner| owner == OWNER);
    if !owned {
        return false;
    }
    // A slot is always checked out detached, so HEAD is the bare commit id.
    let Ok(head) = fs::read_to_string(git_dir.join("HEAD")) else {
        return false;
    };
    let head = head.trim();
    if !matches!(head.len(), 40 | 64) || !head.bytes().all(|b| b.is_ascii_hexdigit()) {
        return false;
    }
    let git_dir_arg = format!("--git-dir={}", common.display());
    git(
        common,
        &[
            &git_dir_arg,
            "cat-file",
            "-e",
            &format!("{head}^{{commit}}"),
        ],
    )
    .is_ok()
}

/// Create or adopt this repository's inspection pool and return its path.
///
/// The pool is derived from the common Git directory so that every linked
/// worktree of a repository leases from one bounded set of slots, but it is
/// placed next to it rather than inside it: a slot is Orbit scratch holding a
/// working tree, not authoritative Git metadata. The policy's `.orbit/**`
/// write deny already covers the pool for any concurrent activity that can
/// write the repository root.
fn prepare_pool(common: &Path) -> Result<PathBuf, DispatchError> {
    let repo_root = common.parent().ok_or_else(|| {
        permanent("source repository has no directory containing its Git metadata")
    })?;
    let mut pool = repo_root.to_path_buf();
    for component in [".orbit", "state", POOL_DIR] {
        pool.push(component);
        directory(&pool).map_err(io_failure)?;
    }
    Ok(pool)
}

/// Retire the pool this module used to keep inside the common Git directory.
///
/// A checkout abandoned there by a crash is no longer reclaimed by the current
/// pool, and would sit under authoritative Git metadata forever — where host
/// protection refuses its tracked symlinks and so blocks every sandboxed
/// activity in the repository. Only slots still carrying this module's owner
/// marker are removed; anything else is left for an operator to inspect.
fn retire_legacy_pool(common: &Path) -> io::Result<()> {
    let legacy = common.join(LEGACY_POOL_DIR);
    reject_symlink(&legacy)?;
    if !legacy.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(&legacy)? {
        let slot = entry?.path();
        reject_symlink(&slot)?;
        let marker = slot.join("owner");
        reject_symlink(&marker)?;
        if fs::read_to_string(&marker).unwrap_or_default() != OWNER {
            return Err(io::Error::other(format!(
                "unowned entry in the retired inspection pool: {}",
                slot.display()
            )));
        }
        // A slot an older build still leases is cleaning up after itself.
        let lock_path = slot.join("lease");
        reject_symlink(&lock_path)?;
        let lease = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        match lease.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Ok(()),
            Err(TryLockError::Error(error)) => return Err(error),
        }

        remove_checkout(&slot.join("checkout"))?;
        fs::remove_dir_all(&slot)?;
    }
    fs::remove_dir(&legacy)
}

fn remove_checkout(root: &Path) -> io::Result<()> {
    reject_symlink(root)?;
    if !root.exists() {
        return Ok(());
    }
    // A gitfile belongs to a registered worktree, which this module never owns.
    let git_dir = root.join(".git");
    reject_symlink(&git_dir)?;
    if git_dir.is_file() {
        return Err(io::Error::other(
            "refusing to remove a registered worktree from an inspection slot",
        ));
    }
    fs::remove_dir_all(root)
}

fn directory(path: &Path) -> io::Result<()> {
    reject_symlink(path)?;
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(error) => Err(error),
    }
}

fn reject_symlink(path: &Path) -> io::Result<()> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::other(
            "inspection resource must not be a symlink",
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn git(root: &Path, args: &[&str]) -> Result<String, DispatchError> {
    git_within(root, GIT_LOCAL_TIMEOUT, args)
}

/// [`git`] under an explicit `deadline`, for a command that copies or
/// materializes a whole checkout.
fn git_within(root: &Path, deadline: Duration, args: &[&str]) -> Result<String, DispatchError> {
    let output = git_output(root, deadline, args)?;
    if !output.success {
        return Err(retryable(format!(
            "git {}: {}",
            args.join(" "),
            output.stderr.trim()
        )));
    }
    Ok(output.stdout)
}

/// Confirm `revision` names a commit. A name that does not resolve is
/// permanent. Opening the repository or reading its objects can fail
/// transiently, and those errors stay retryable.
fn require_commit(source: &Path, revision: &str) -> Result<(), DispatchError> {
    let spec = format!("{revision}^{{commit}}");
    let output = git_output(
        source,
        GIT_LOCAL_TIMEOUT,
        &["rev-parse", "--verify", "-q", &spec],
    )?;
    if output.success {
        return Ok(());
    }
    if missing_commit(&output) {
        let stderr = output.stderr.trim();
        return Err(permanent(if stderr.is_empty() {
            format!("inspection revision {revision} does not name a commit")
        } else {
            format!("inspection revision {revision} does not name a commit: {stderr}")
        }));
    }
    Err(retryable(format!(
        "git rev-parse --verify -q {spec}: {}",
        output.stderr.trim()
    )))
}

/// `rev-parse --verify -q <rev>^{commit}` fails with empty stderr when the
/// name is absent. A type mismatch names the object. Repository-level
/// failures write a fatal line and are not a missing commit.
fn missing_commit(output: &GitCommandOutput) -> bool {
    let stderr = output.stderr.trim();
    if stderr.is_empty() {
        return true;
    }
    let stderr = stderr.to_ascii_lowercase();
    if stderr.contains("not a git repository")
        || stderr.contains("permission denied")
        || stderr.contains("resource temporarily unavailable")
        || stderr.contains("input/output error")
        || stderr.contains("unable to access")
        || stderr.contains(".lock")
    {
        return false;
    }
    stderr.contains("expected commit type") || stderr.contains("not a valid object name")
}

fn git_output(
    root: &Path,
    deadline: Duration,
    args: &[&str],
) -> Result<GitCommandOutput, DispatchError> {
    let mut argv = vec!["-c", "core.hooksPath=/dev/null", "-c", "gc.auto=0"];
    argv.extend_from_slice(args);
    run_git_within(root, &argv, deadline).map_err(|error| retryable(error.to_string()))
}

fn permanent(message: impl std::fmt::Display) -> DispatchError {
    DispatchError::CliInvocationPermanent(format!("source inspection: {message}"))
}

fn retryable(message: impl std::fmt::Display) -> DispatchError {
    DispatchError::CliInvocationFailed(format!("source inspection: {message}"))
}

fn io_failure(error: io::Error) -> DispatchError {
    retryable(error)
}
