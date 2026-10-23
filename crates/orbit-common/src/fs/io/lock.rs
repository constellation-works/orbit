use std::cell::RefCell;
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::private::{create_private_dir_all, write_access_error_message};
use crate::fs::file_lock::{
    FileLockOptions, acquire_exclusive_file_lock, acquire_shared_file_lock,
};

thread_local! {
    /// Lock files this thread currently holds, by lock-file path.
    ///
    /// ORB-10988: `flock(2)` is owned by the open file description, not by the
    /// process or thread, so a nested `with_exclusive_file_lock` on the same
    /// path opens a *second* descriptor and blocks against the outer one —
    /// a self-deadlock, not a re-entry. Tracking held paths per thread makes
    /// the helper re-entrant, which is what lets a caller hold a task lock
    /// across a read-modify-write whose inner writes lock the same file.
    static HELD_LOCK_PATHS: RefCell<HashSet<PathBuf>> = RefCell::new(HashSet::new());
}

/// Removes `path` from this thread's held set on drop, including on unwind.
struct HeldLockPath(PathBuf);

impl Drop for HeldLockPath {
    fn drop(&mut self) {
        HELD_LOCK_PATHS.with(|held| {
            held.borrow_mut().remove(&self.0);
        });
    }
}

/// Registers `path` as held by this thread, or returns `None` when this thread
/// already holds it (the caller then runs `op` under the outer lock).
fn claim_lock_path(path: &Path) -> Option<HeldLockPath> {
    HELD_LOCK_PATHS.with(|held| {
        held.borrow_mut()
            .insert(path.to_path_buf())
            .then(|| HeldLockPath(path.to_path_buf()))
    })
}

/// Run `op` while holding an exclusive advisory flock on a sibling lock
/// file of `target_path` (`.<filename>.lock`). Creates the parent directory
/// if missing. The lock is released when this function returns.
///
/// The lock is re-entrant per thread: a nested call for the same lock path
/// runs `op` directly under the outermost acquisition instead of deadlocking
/// on a second descriptor. Cross-thread and cross-process callers wait up to
/// [`crate::fs::file_lock::DEFAULT_FILE_LOCK_TIMEOUT`] on the flock, including readers holding
/// [`with_shared_file_lock`] on the same target.
///
/// The closure returns `Result<T, E>` where any filesystem error hit while
/// acquiring the lock is folded into `E` via `From<std::io::Error>` —
/// callers returning `OrbitError`, `io::Error`, or any error type that
/// implements `From<io::Error>` compose directly.
///
/// `label` prefixes error messages for diagnosability when the lock path
/// alone isn't enough context.
pub fn with_exclusive_file_lock<T, E, F>(target_path: &Path, label: &str, op: F) -> Result<T, E>
where
    F: FnOnce() -> Result<T, E>,
    E: From<io::Error>,
{
    with_exclusive_file_lock_options(target_path, label, FileLockOptions::default(), op)
}

/// [`with_exclusive_file_lock`] with an explicit, testable acquisition policy.
pub fn with_exclusive_file_lock_options<T, E, F>(
    target_path: &Path,
    label: &str,
    options: FileLockOptions,
    op: F,
) -> Result<T, E>
where
    F: FnOnce() -> Result<T, E>,
    E: From<io::Error>,
{
    with_exclusive_file_lock_named_options(target_path, label, options, op)
}

/// [`with_exclusive_file_lock`] with an explicit, testable acquisition policy.
///
/// The sibling lock file is always `.{file_name}.lock`.
pub fn with_exclusive_file_lock_named_options<T, E, F>(
    target_path: &Path,
    label: &str,
    options: FileLockOptions,
    op: F,
) -> Result<T, E>
where
    F: FnOnce() -> Result<T, E>,
    E: From<io::Error>,
{
    let parent = target_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cannot determine parent for '{}'", target_path.display()),
        )
    })?;

    // Create the parent before resolving, not after. `resolved_lock_path`
    // canonicalizes through the parent and falls back to the literal path when
    // the parent is missing, so resolving first made the key depend on whether
    // this call happened to be the one that created the directory: an outer
    // call keyed the literal path, created the parent, and the nested call then
    // canonicalized to a different key, missed the lock it already held, and
    // blocked on a second descriptor to the same file. Creating the parent
    // first makes the parent always resolvable, so every call in a nest agrees
    // on the key. Under a path that canonicalizes to itself the two orders are
    // indistinguishable, which is why this only ever deadlocked where a symlink
    // sat above the target.
    create_private_dir_all(parent).map_err(|e| classify_lock_io(parent, e))?;
    let lock_path = resolved_lock_path(target_path)?;
    let Some(_held) = claim_lock_path(&lock_path) else {
        return op();
    };
    let _lock = acquire_exclusive_file_lock(&lock_path, label, options)?;

    op()
}

/// An exclusive advisory lock held for as long as the guard
/// [`hold_exclusive_file_lock_options`] returns, rather than for one closure.
///
/// For a holder that must release an enclosing lock and keep this one: the
/// closure helpers can only release locks in the reverse order they took them.
/// The guard is keyed and re-entrant exactly like
/// [`with_exclusive_file_lock_options`], so while it lives nested lock calls
/// on the same target on this thread run under it. The re-entrancy record is
/// per thread, so the guard is not `Send`.
#[must_use = "the advisory lock is released as soon as the guard is dropped"]
pub struct HeldFileLock {
    // Declared before `held`, so the lock is released before this thread
    // stops recording it as held, as the closure helpers do.
    lock: Option<crate::fs::file_lock::FileLockGuard>,
    held: Option<HeldLockPath>,
    not_send: std::marker::PhantomData<*const ()>,
}

impl HeldFileLock {
    /// Whether this thread already held the lock when the guard was taken,
    /// so dropping the guard releases nothing.
    pub fn is_reentry(&self) -> bool {
        self.held.is_none()
    }
}

impl std::fmt::Debug for HeldFileLock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HeldFileLock")
            .field("lock", &self.lock)
            .field("reentry", &self.is_reentry())
            .finish()
    }
}

/// Take the exclusive lock [`with_exclusive_file_lock_options`] takes for
/// `target_path`, held until the returned guard drops. A request made while
/// this thread already holds that lock returns a guard that releases nothing.
pub fn hold_exclusive_file_lock_options(
    target_path: &Path,
    label: &str,
    options: FileLockOptions,
) -> io::Result<HeldFileLock> {
    let parent = target_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cannot determine parent for '{}'", target_path.display()),
        )
    })?;
    // Created before resolving, for the reason the closure helper gives.
    create_private_dir_all(parent).map_err(|e| classify_lock_io(parent, e))?;
    let lock_path = resolved_lock_path(target_path)?;
    let Some(held) = claim_lock_path(&lock_path) else {
        return Ok(HeldFileLock {
            lock: None,
            held: None,
            not_send: std::marker::PhantomData,
        });
    };
    let lock = acquire_exclusive_file_lock(&lock_path, label, options)?;
    Ok(HeldFileLock {
        lock: Some(lock),
        held: Some(held),
        not_send: std::marker::PhantomData,
    })
}

/// Run `op` while holding a *shared* advisory flock on the same sibling lock
/// file [`with_exclusive_file_lock`] uses, so readers exclude writers of that
/// target while staying concurrent with each other.
///
/// This is the read half of a multi-file critical section (ORB-11349): a task
/// bundle's transition appends to one file and republishes another, so a
/// reader that assembles both without coordination can pair a new event log
/// with an old envelope and report that mismatch as corruption.
///
/// Three properties keep this usable from read-only surfaces:
///
/// - The parent directory is never created. A read of something that does not
///   exist must not materialize it, and a missing parent also means no writer
///   can be holding anything inside it, so `op` runs directly.
/// - Acquisition is best effort. A store on a read-only mount, or any
///   filesystem that refuses the lock file, still serves the read unlocked
///   rather than failing it — the same exposure as before this lock existed.
///   Active contention waits up to the configured deadline and returns a typed
///   timeout instead of reading through a live writer.
/// - Re-entrancy is shared with the exclusive variant, so a read nested inside
///   a writer's own critical section runs directly instead of deadlocking on a
///   second descriptor.
///
/// Do not take the *write* lock for a target inside a read lock on that same
/// target. A nested request never upgrades the outer acquisition, so the
/// mutation would run under a shared lock that concurrent readers also hold.
pub fn with_shared_file_lock<T, E, F>(target_path: &Path, label: &str, op: F) -> Result<T, E>
where
    F: FnOnce() -> Result<T, E>,
    E: From<io::Error>,
{
    with_shared_file_lock_options(target_path, label, FileLockOptions::default(), op)
}

/// [`with_shared_file_lock`] with an explicit, testable acquisition policy.
pub fn with_shared_file_lock_options<T, E, F>(
    target_path: &Path,
    label: &str,
    options: FileLockOptions,
    op: F,
) -> Result<T, E>
where
    F: FnOnce() -> Result<T, E>,
    E: From<io::Error>,
{
    let Some(parent) = target_path.parent() else {
        return op();
    };
    if !parent.is_dir() {
        return op();
    }
    let lock_path = resolved_lock_path(target_path)?;
    let Some(_held) = claim_lock_path(&lock_path) else {
        return op();
    };
    let _lock_file = match acquire_shared_file_lock(&lock_path, label, options) {
        Ok(file) => Some(file),
        Err(error) if error.kind() == io::ErrorKind::TimedOut => return Err(E::from(error)),
        Err(error) => {
            crate::tracing::debug!(
                target: "orbit.common.fs",
                lock_path = %lock_path.display(),
                label,
                error = %error,
                "shared lock unavailable; reading without writer coordination",
            );
            None
        }
    };

    op()
}

/// The lock path to open and to key re-entrancy on, resolved through symlinks
/// where the parent directory already exists.
///
/// Orbit reaches one task bundle by more than one route — the canonical store
/// path and the checkout projection that links to it — so keying re-entrancy
/// on the literal path would let a nested call miss its own outer lock and
/// deadlock on a second descriptor to the same file. Resolving the parent
/// collapses those routes to one key. An unresolvable parent means the
/// directory does not exist yet, so nothing can be holding a lock inside it.
fn resolved_lock_path(target_path: &Path) -> io::Result<PathBuf> {
    let file_name = target_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("path '{}' has no file name", target_path.display()),
            )
        })?;
    let lock_path = target_path.with_file_name(format!(".{file_name}.lock"));
    let Some(file_name) = lock_path.file_name() else {
        return Ok(lock_path);
    };
    match lock_path.parent().map(fs::canonicalize) {
        Some(Ok(parent)) => Ok(parent.join(file_name)),
        _ => Ok(lock_path),
    }
}
pub(crate) fn classify_lock_io(path: &Path, err: io::Error) -> io::Error {
    match write_access_error_message(path, &err) {
        Some(message) => io::Error::new(err.kind(), message),
        None => err,
    }
}

pub(crate) fn classify_or_wrap_lock_io(
    path: &Path,
    err: io::Error,
    fallback: impl FnOnce(&io::Error) -> String,
) -> io::Error {
    match write_access_error_message(path, &err) {
        Some(message) => io::Error::new(err.kind(), message),
        None => io::Error::other(fallback(&err)),
    }
}
