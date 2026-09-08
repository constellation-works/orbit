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

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use orbit_common::fs::git::run_git;
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
                    .map_err(|error| failure(error.to_string()))?;
                if output.success && output.stdout.trim() == "true" {
                    return Err(failure(
                        "a Git workspace requires a pinned inspection_revision",
                    ));
                }
            }
            return Ok(None);
        }
        let revision = value
            .as_str()
            .ok_or_else(|| failure("inspection_revision must be a commit id"))?;
        if !matches!(revision.len(), 40 | 64) || !revision.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(failure("inspection_revision must be a full commit id"));
        }
        if let Some(source_revision) = input.get("source_revision").and_then(Value::as_str)
            && source_revision != revision
        {
            return Err(failure(
                "inspection_revision differs from the prepared source_revision",
            ));
        }
        if fs_profile != Some("reviewer") {
            return Err(failure(
                "source inspection requires the reviewer filesystem profile",
            ));
        }
        let source = source.ok_or_else(|| failure("source inspection requires a workspace"))?;
        Self::create(source, revision).map(Some)
    }

    fn create(source: &Path, revision: &str) -> Result<Self, DispatchError> {
        git(
            source,
            &["cat-file", "-e", &format!("{revision}^{{commit}}")],
        )?;
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
                    return Err(failure("inspection slot has an unrecognized owner"));
                }
                remove_checkout(&root).map_err(io_failure)?;
            } else {
                if root.symlink_metadata().is_ok() {
                    return Err(failure("refusing to remove an unowned inspection checkout"));
                }
                fs::write(&marker, OWNER).map_err(io_failure)?;
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
            // Fetch only this immutable revision and its history into private
            // objects. Avoid alternates so sandboxed Git never needs the primary.
            git(
                &inspection.root,
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
            git(
                &inspection.root,
                &["checkout", "--quiet", "--detach", revision],
            )?;
            inspection.verify()?;
            return Ok(inspection);
        }
        Err(failure(
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
            return Err(failure(
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

/// Create or adopt this repository's inspection pool and return its path.
///
/// The pool is derived from the common Git directory so that every linked
/// worktree of a repository leases from one bounded set of slots, but it is
/// placed next to it rather than inside it: a slot is Orbit scratch holding a
/// working tree, not authoritative Git metadata. The policy's `.orbit/**`
/// write deny already covers the pool for any concurrent activity that can
/// write the repository root.
fn prepare_pool(common: &Path) -> Result<PathBuf, DispatchError> {
    let repo_root = common
        .parent()
        .ok_or_else(|| failure("source repository has no directory containing its Git metadata"))?;
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
    let mut argv = vec!["-c", "core.hooksPath=/dev/null", "-c", "gc.auto=0"];
    argv.extend_from_slice(args);
    let output = run_git(root, &argv).map_err(|error| failure(error.to_string()))?;
    if !output.success {
        return Err(failure(format!(
            "git {}: {}",
            args.join(" "),
            output.stderr.trim()
        )));
    }
    Ok(output.stdout)
}

fn failure(message: impl std::fmt::Display) -> DispatchError {
    DispatchError::CliInvocationFailed(format!("source inspection: {message}"))
}

fn io_failure(error: io::Error) -> DispatchError {
    failure(error)
}
