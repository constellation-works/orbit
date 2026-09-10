//! Bounded advisory file-lock acquisition and holder diagnostics.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use super::io::{
    classify_lock_io, classify_or_wrap_lock_io, create_private_dir_all, open_private_file,
};

/// Default upper bound for an advisory file-lock acquisition.
pub const DEFAULT_FILE_LOCK_TIMEOUT: Duration = Duration::from_secs(30);

const DEFAULT_FILE_LOCK_WARN_AFTER: Duration = Duration::from_secs(3);
const FILE_LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(50);

/// Deadline and warning policy for one advisory file-lock acquisition.
#[derive(Debug, Clone, Copy)]
pub struct FileLockOptions {
    /// Fail acquisition after this duration.
    pub timeout: Duration,
    /// Emit one contention warning after this duration.
    pub warn_after: Duration,
}

impl Default for FileLockOptions {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_FILE_LOCK_TIMEOUT,
            warn_after: DEFAULT_FILE_LOCK_WARN_AFTER,
        }
    }
}

/// Advisory metadata recorded by an exclusive lock holder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileLockHolderInfo {
    /// Process ID of the exclusive holder that wrote this metadata.
    pub pid: u32,
    /// RFC 3339 acquisition timestamp.
    pub acquired_at: String,
    /// Human-readable operation label.
    pub label: String,
}

/// Structured details for an advisory lock acquisition that exceeded its deadline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileLockTimeout {
    /// Exact, stable lock file that could not be acquired.
    pub lock_path: PathBuf,
    /// Human-readable label of the waiting operation.
    pub label: String,
    /// Injected acquisition deadline in milliseconds.
    pub timeout_ms: u64,
    /// Advisory exclusive-holder metadata, when a complete record was readable.
    pub holder: Option<FileLockHolderInfo>,
}

impl std::fmt::Display for FileLockTimeout {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "timed out after {}ms acquiring {} lock '{}'",
            self.timeout_ms,
            self.label,
            self.lock_path.display()
        )?;
        if let Some(holder) = &self.holder {
            write!(
                formatter,
                " (held by pid {} since {}, op: {})",
                holder.pid, holder.acquired_at, holder.label
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for FileLockTimeout {}

/// RAII guard for a directly acquired advisory lock.
#[derive(Debug)]
#[must_use = "the advisory lock is released as soon as the guard is dropped"]
pub struct FileLockGuard {
    file: File,
    clear_holder_on_drop: bool,
}

impl Drop for FileLockGuard {
    fn drop(&mut self) {
        if self.clear_holder_on_drop {
            let _ = clear_file_lock_holder(&self.file);
        }
    }
}

/// Acquire an exclusive advisory lock at the exact `lock_path`.
///
/// This is the common mechanism behind target-derived locks and store locks.
/// It preserves the lock file and only clears holder metadata on clean drop,
/// so queued descriptors never switch to a replacement inode.
pub fn acquire_exclusive_file_lock(
    lock_path: &Path,
    label: &str,
    options: FileLockOptions,
) -> io::Result<FileLockGuard> {
    let parent = lock_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cannot determine lock parent for '{}'", lock_path.display()),
        )
    })?;
    create_private_dir_all(parent).map_err(|error| classify_lock_io(parent, error))?;

    let lock_file = open_lock_file(lock_path, label)?;
    acquire_file_lock(lock_file, lock_path, label, options, true)
}

/// Attempt one exclusive acquisition at the exact `lock_path` without waiting.
pub fn try_acquire_exclusive_file_lock(
    lock_path: &Path,
    label: &str,
) -> io::Result<Option<FileLockGuard>> {
    let parent = lock_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cannot determine lock parent for '{}'", lock_path.display()),
        )
    })?;
    create_private_dir_all(parent).map_err(|error| classify_lock_io(parent, error))?;

    let lock_file = open_lock_file(lock_path, label)?;
    match FileExt::try_lock_exclusive(&lock_file) {
        Ok(()) => {
            write_file_lock_holder(&lock_file, label);
            Ok(Some(FileLockGuard {
                file: lock_file,
                clear_holder_on_drop: true,
            }))
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(classify_or_wrap_lock_io(lock_path, error, |error| {
            format!("lock {label} '{}': {error}", lock_path.display())
        })),
    }
}

pub(crate) fn acquire_shared_file_lock(
    lock_path: &Path,
    label: &str,
    options: FileLockOptions,
) -> io::Result<FileLockGuard> {
    let lock_file = open_lock_file(lock_path, label)?;
    acquire_file_lock(lock_file, lock_path, label, options, false)
}

/// Read advisory holder metadata. Missing, empty, torn, and legacy files are
/// intentionally reported as no metadata because the OS lock is authoritative.
pub fn read_file_lock_holder(lock_path: &Path) -> Option<FileLockHolderInfo> {
    read_file_lock_holder_with_hook(lock_path, |_| {})
}

/// Test-only seam that runs `before_open` between path resolution and the
/// no-follow open, so a test can deterministically swap the resolved final
/// component for a symlink and prove the open still rejects it [ORB-12029].
#[cfg(test)]
pub(crate) fn read_file_lock_holder_after_resolve<F>(
    lock_path: &Path,
    before_open: F,
) -> Option<FileLockHolderInfo>
where
    F: FnOnce(&Path),
{
    read_file_lock_holder_with_hook(lock_path, before_open)
}

fn read_file_lock_holder_with_hook(
    lock_path: &Path,
    before_open: impl FnOnce(&Path),
) -> Option<FileLockHolderInfo> {
    let mut file = open_lock_holder_file(lock_path, before_open)?;
    let mut raw = String::new();
    file.read_to_string(&mut raw).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Resolve `lock_path`'s parent directory to its canonical form.
///
/// `lock_path` reaches this crate as a caller-selected value (a task lock, a
/// store lock) with no upstream containment check. Canonicalizing only the
/// parent — not the final component — keeps the read confined to the
/// resolved parent directory while still accepting a trusted parent alias
/// (for example a checkout projection symlink) [ORB-11953]. Deciding whether
/// the final component itself is safe to read is [`open_lock_holder_file`]'s
/// job, not this function's: resolving that here and handing back a path
/// left a window between this check and the caller's open where the final
/// component could be swapped for a symlink [ORB-12029].
fn validated_lock_holder_parent(lock_path: &Path) -> Option<PathBuf> {
    lock_path.parent()?.canonicalize().ok()
}

/// Open `lock_path`'s final component for a read-only diagnostic read
/// without following a symlink planted there.
///
/// The pathname `symlink_metadata` check below is a fast rejection for the
/// common case (missing file, directory, or already-a-symlink) so this
/// avoids blocking on an exotic node such as a FIFO; it is *not* the security
/// boundary. That boundary is the open immediately after: on Unix,
/// `O_NOFOLLOW` makes the open itself fail if the final component is a
/// symlink, and the follow-up `metadata()` call is an `fstat` on the
/// already-open descriptor, re-checking the file that was actually opened
/// rather than a path that could have changed again. Folding the check and
/// the open into one function, with the open re-validating its own
/// descriptor, closes the window a caller-visible "validated path" handed to
/// a separate `File::open` left open [ORB-12029]. `None` covers "nothing to
/// read", matching this function's existing no-metadata-on-missing/malformed
/// -target semantics.
///
/// This secures only the final path component. [`validated_lock_holder_parent`]
/// resolving the parent supports a trusted parent alias; it does not claim to
/// stop a party who can write to that parent from renaming or replacing the
/// lock file's directory entry through some other, ancestor-level race —
/// only the leaf-symlink swap this closes.
///
/// The no-follow open is atomic against the leaf swap on Unix (`O_NOFOLLOW`)
/// and on Windows (`FILE_FLAG_OPEN_REPARSE_POINT`, which opens a reparse
/// point itself instead of its target). On any other platform the open
/// follows a symlink normally, so the pathname pre-check above is the only
/// protection and a swap landing between that check and the open is not
/// covered there.
fn open_lock_holder_file(lock_path: &Path, before_open: impl FnOnce(&Path)) -> Option<File> {
    let file_name = lock_path.file_name()?;
    let canonical_parent = validated_lock_holder_parent(lock_path)?;
    let candidate = canonical_parent.join(file_name);

    match std::fs::symlink_metadata(&candidate) {
        Ok(metadata) if metadata.is_file() => {}
        _ => return None,
    }

    before_open(&candidate);

    let mut options = OpenOptions::new();
    options.read(true);
    apply_read_only_no_follow(&mut options);
    let file = options.open(&candidate).ok()?;
    let metadata = file.metadata().ok()?;
    metadata.is_file().then_some(file)
}

#[cfg(unix)]
fn apply_read_only_no_follow(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(libc::O_NOFOLLOW);
}

#[cfg(windows)]
fn apply_read_only_no_follow(options: &mut OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
}

#[cfg(not(any(unix, windows)))]
fn apply_read_only_no_follow(_options: &mut OpenOptions) {}

fn open_lock_file(lock_path: &Path, label: &str) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    open_private_file(lock_path, &mut options).map_err(|error| {
        classify_or_wrap_lock_io(lock_path, error, |error| {
            format!("open {label} lock '{}': {error}", lock_path.display())
        })
    })
}

fn acquire_file_lock(
    lock_file: File,
    lock_path: &Path,
    label: &str,
    options: FileLockOptions,
    exclusive: bool,
) -> io::Result<FileLockGuard> {
    let started = Instant::now();
    let mut warned = false;

    loop {
        let acquisition = if exclusive {
            FileExt::try_lock_exclusive(&lock_file)
        } else {
            FileExt::try_lock_shared(&lock_file)
        };
        match acquisition {
            Ok(()) => {
                if exclusive {
                    write_file_lock_holder(&lock_file, label);
                }
                return Ok(FileLockGuard {
                    file: lock_file,
                    clear_holder_on_drop: exclusive,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let elapsed = started.elapsed();
                if elapsed >= options.timeout {
                    let timeout = FileLockTimeout {
                        lock_path: lock_path.to_path_buf(),
                        label: label.to_string(),
                        timeout_ms: duration_millis(options.timeout),
                        holder: read_file_lock_holder(lock_path),
                    };
                    return Err(io::Error::new(io::ErrorKind::TimedOut, timeout));
                }
                if !warned && elapsed >= options.warn_after {
                    warned = true;
                    warn_for_contention(lock_path, label, elapsed);
                }
                std::thread::sleep(FILE_LOCK_RETRY_INTERVAL.min(options.timeout - elapsed));
            }
            Err(error) => {
                return Err(classify_or_wrap_lock_io(lock_path, error, |error| {
                    format!("lock {label} '{}': {error}", lock_path.display())
                }));
            }
        }
    }
}

fn warn_for_contention(lock_path: &Path, label: &str, elapsed: Duration) {
    let holder = read_file_lock_holder(lock_path)
        .map(|holder| {
            format!(
                "pid {} since {} ({})",
                holder.pid, holder.acquired_at, holder.label
            )
        })
        .unwrap_or_else(|| "unknown".to_string());
    crate::tracing::warn!(
        target: "orbit.common.fs.file_lock",
        lock_path = %lock_path.display(),
        label,
        waited_ms = duration_millis(elapsed),
        holder,
        "still waiting for advisory file lock; a holder may be hung",
    );
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

fn write_file_lock_holder(file: &File, label: &str) {
    let holder = FileLockHolderInfo {
        pid: std::process::id(),
        acquired_at: chrono::Utc::now().to_rfc3339(),
        label: label.to_string(),
    };
    let _ = write_file_lock_holder_inner(file, &holder);
}

fn write_file_lock_holder_inner(mut file: &File, holder: &FileLockHolderInfo) -> io::Result<()> {
    let encoded = serde_json::to_vec(holder).map_err(io::Error::other)?;
    file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    file.write_all(&encoded)?;
    file.flush()
}

fn clear_file_lock_holder(mut file: &File) -> io::Result<()> {
    file.set_len(0)?;
    file.flush()
}
