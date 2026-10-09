//! Bounded advisory file-lock acquisition and holder diagnostics.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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

/// How long a single-shot acquisition waits out a refusal that no holder
/// claims — see [`try_acquire_exclusive_file_lock`]. Sized for a forked child
/// to reach `execve` on a loaded host, not for a holder to finish its work.
const UNCLAIMED_LOCK_GRACE: Duration = Duration::from_secs(2);

/// Infix after the lock file's name that names a shared holder's record
/// beside it: `<lock>.holder-<pid>-<seq>`. Records are files, not a
/// directory, because a lock's parent may be a store partition whose
/// subdirectories are read as its records; a dot-prefixed lock's records stay
/// dot-prefixed infrastructure beside it.
const SHARED_HOLDER_INFIX: &str = ".holder-";

/// Most shared holders one diagnostic line names before summarizing the rest.
const MAX_NAMED_SHARED_HOLDERS: usize = 8;

static SHARED_HOLDER_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Deadline, warning and diagnostic policy for one advisory file-lock
/// acquisition.
#[derive(Debug, Clone, Copy)]
pub struct FileLockOptions {
    /// Fail acquisition after this duration.
    pub timeout: Duration,
    /// Emit one contention warning after this duration.
    pub warn_after: Duration,
    /// Record each shared holder beside the lock file, so a waiter blocked by
    /// readers can name them. An exclusive holder is always recorded. Costs a
    /// file create and remove per shared acquisition, so it is meant for
    /// coordination locks, not per-record locks taken by every read.
    pub record_shared_holders: bool,
    /// Log the label and held duration when a holder releases the lock after
    /// holding it at least this long.
    pub warn_held_after: Option<Duration>,
}

impl Default for FileLockOptions {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_FILE_LOCK_TIMEOUT,
            warn_after: DEFAULT_FILE_LOCK_WARN_AFTER,
            record_shared_holders: false,
            warn_held_after: None,
        }
    }
}

/// Advisory metadata recorded by a lock holder: in the lock file by an
/// exclusive holder, beside it by a shared one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileLockHolderInfo {
    /// Process ID of the holder that wrote this metadata.
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
    /// Live shared holders, for a lock whose readers record themselves
    /// ([`FileLockOptions::record_shared_holders`]).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub shared_holders: Vec<FileLockHolderInfo>,
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
        } else if !self.shared_holders.is_empty() {
            write!(
                formatter,
                " (held shared by {})",
                describe_shared_holders(&self.shared_holders)
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
    /// This holder's record beside the lock, removed on release.
    shared_record: Option<PathBuf>,
    /// Logs the hold on release once it passes the configured threshold.
    hold: Option<HoldReport>,
}

impl Drop for FileLockGuard {
    fn drop(&mut self) {
        if let Some(record) = &self.shared_record {
            let _ = std::fs::remove_file(record);
        }
        if self.clear_holder_on_drop {
            let _ = clear_file_lock_holder(&self.file);
        }
        if let Some(hold) = &self.hold {
            hold.report();
        }
    }
}

/// What a holder needs to say on release that it held the lock too long.
#[derive(Debug)]
struct HoldReport {
    lock_path: PathBuf,
    label: String,
    exclusive: bool,
    acquired: Instant,
    warn_after: Duration,
}

impl HoldReport {
    fn report(&self) {
        let held = self.acquired.elapsed();
        if held < self.warn_after {
            return;
        }
        crate::tracing::warn!(
            target: "orbit.common.fs.file_lock",
            lock_path = %self.lock_path.display(),
            label = %self.label,
            mode = if self.exclusive { "exclusive" } else { "shared" },
            held_ms = duration_millis(held),
            threshold_ms = duration_millis(self.warn_after),
            "advisory file lock held past its threshold",
        );
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

/// Attempt one exclusive acquisition at the exact `lock_path` without queueing
/// behind a live holder. `Ok(None)` means someone else owns the lock and the
/// caller should give up rather than wait.
///
/// Contention is decided from the holder record an owner writes under the
/// lock, not from the raw `flock` refusal alone, because in a process that
/// spawns children the two are not the same thing. A `flock` lock belongs to
/// the *open file description*, and `fork` duplicates the whole descriptor
/// table: a child forked by any thread while this lock was held goes on
/// holding it until its `execve` closes the descriptor. `O_CLOEXEC` bounds
/// that window but cannot remove it, and on a loaded host the forked child can
/// take milliseconds to be scheduled. A caller that releases the lock and
/// immediately re-acquires it is then refused by a descriptor that belongs to
/// no holder at all (ORB-12532).
///
/// A refusal carrying no holder record is exactly that case: the previous
/// owner cleared its record when it dropped its guard, and a new owner writes
/// one as soon as it acquires. Such a refusal is waited out for
/// `UNCLAIMED_LOCK_GRACE` and reported as contention only if it outlives it.
/// A refusal a holder does claim returns `Ok(None)` immediately, so a caller
/// whose correct response to real contention is "exit" never queues behind a
/// live pass.
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
    let started = Instant::now();
    loop {
        match FileExt::try_lock_exclusive(&lock_file) {
            Ok(()) => {
                write_file_lock_holder(&lock_file, label);
                return Ok(Some(FileLockGuard {
                    file: lock_file,
                    clear_holder_on_drop: true,
                    shared_record: None,
                    hold: None,
                }));
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if read_file_lock_holder(lock_path).is_some() {
                    return Ok(None);
                }
                let elapsed = started.elapsed();
                if elapsed >= UNCLAIMED_LOCK_GRACE {
                    warn_for_unclaimed_refusal(lock_path, label, elapsed);
                    return Ok(None);
                }
                std::thread::sleep(FILE_LOCK_RETRY_INTERVAL.min(UNCLAIMED_LOCK_GRACE - elapsed));
            }
            Err(error) => {
                return Err(classify_or_wrap_lock_io(lock_path, error, |error| {
                    format!("lock {label} '{}': {error}", lock_path.display())
                }));
            }
        }
    }
}

/// One line for a refusal that outlived [`UNCLAIMED_LOCK_GRACE`] without any
/// holder claiming it: either a descriptor inherited by a child that has still
/// not exec'd, or a holder that took the lock and never recorded itself. The
/// caller is about to treat it as contention, so the reason it did must be
/// attributable from a log.
fn warn_for_unclaimed_refusal(lock_path: &Path, label: &str, elapsed: Duration) {
    crate::tracing::warn!(
        target: "orbit.common.fs.file_lock",
        lock_path = %lock_path.display(),
        label,
        waited_ms = duration_millis(elapsed),
        "advisory file lock refused with no holder recorded; treating as contention",
    );
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

/// Read the records shared holders keep beside `lock_path`, oldest first.
///
/// Only a lock acquired with [`FileLockOptions::record_shared_holders`] has
/// any. A record whose process is gone is a crashed holder's leftover — the
/// OS released its lock with the process — so it is skipped and removed.
/// Torn and unreadable records are skipped, like the exclusive record.
fn read_shared_file_lock_holders(lock_path: &Path) -> Vec<FileLockHolderInfo> {
    let (Some(parent), Some(prefix)) = (lock_path.parent(), shared_holder_prefix(lock_path)) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut holders = Vec::new();
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(&prefix))
        {
            continue;
        }
        let path = entry.path();
        let Some(file) = open_lock_holder_file(&path, |_| {}) else {
            continue;
        };
        let Some(holder) = read_holder_record(file) else {
            continue;
        };
        if holder_process_gone(holder.pid) {
            let _ = std::fs::remove_file(&path);
        } else {
            holders.push(holder);
        }
    }
    holders.sort_by(|left, right| left.acquired_at.cmp(&right.acquired_at));
    holders
}

/// Whether a recorded holder's process has exited. Only Unix can tell; a
/// record elsewhere is kept and its holder named.
fn holder_process_gone(pid: u32) -> bool {
    #[cfg(unix)]
    {
        !crate::process::identity::process_is_alive(pid)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn read_holder_record(mut file: File) -> Option<FileLockHolderInfo> {
    let mut raw = String::new();
    file.read_to_string(&mut raw).ok()?;
    serde_json::from_str(&raw).ok()
}

fn shared_holder_prefix(lock_path: &Path) -> Option<String> {
    let name = lock_path.file_name()?.to_str()?;
    Some(format!("{name}{SHARED_HOLDER_INFIX}"))
}

/// Record this shared holder beside the lock. Best effort, like the
/// exclusive record: the OS lock is authoritative, and a store that refuses
/// the record still serves the read.
fn write_shared_holder_record(lock_path: &Path, label: &str) -> Option<PathBuf> {
    let prefix = shared_holder_prefix(lock_path)?;
    let sequence = SHARED_HOLDER_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = lock_path.with_file_name(format!("{prefix}{}-{sequence}", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    let file = open_private_file(&path, &mut options).ok()?;
    let holder = FileLockHolderInfo {
        pid: std::process::id(),
        acquired_at: chrono::Utc::now().to_rfc3339(),
        label: label.to_string(),
    };
    match write_file_lock_holder_inner(&file, &holder) {
        Ok(()) => Some(path),
        Err(_) => {
            let _ = std::fs::remove_file(&path);
            None
        }
    }
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
    read_holder_record(open_lock_holder_file(lock_path, before_open)?)
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
/// symlink and `O_NONBLOCK` prevents a swapped FIFO from hanging the caller;
/// the follow-up `metadata()` call is an `fstat` on the
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
/// and nonblocking for a swapped FIFO. On Windows,
/// `FILE_FLAG_OPEN_REPARSE_POINT` opens a reparse point itself instead of its
/// target. On any other platform the open
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

    let file = super::open_read_only_no_follow(&candidate).ok()?;
    let metadata = file.metadata().ok()?;
    metadata.is_file().then_some(file)
}

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
                let shared_record = if exclusive {
                    write_file_lock_holder(&lock_file, label);
                    None
                } else if options.record_shared_holders {
                    write_shared_holder_record(lock_path, label)
                } else {
                    None
                };
                return Ok(FileLockGuard {
                    file: lock_file,
                    clear_holder_on_drop: exclusive,
                    shared_record,
                    hold: options.warn_held_after.map(|warn_after| HoldReport {
                        lock_path: lock_path.to_path_buf(),
                        label: label.to_string(),
                        exclusive,
                        acquired: Instant::now(),
                        warn_after,
                    }),
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
                        shared_holders: read_shared_file_lock_holders(lock_path),
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

/// Name whoever holds `lock_path` for a contention diagnostic: the exclusive
/// holder's record, else every live shared holder's, else `unknown`.
fn describe_file_lock_holders(lock_path: &Path) -> String {
    if let Some(holder) = read_file_lock_holder(lock_path) {
        return format!(
            "pid {} since {} ({})",
            holder.pid, holder.acquired_at, holder.label
        );
    }
    let shared = read_shared_file_lock_holders(lock_path);
    if shared.is_empty() {
        return "unknown".to_string();
    }
    format!("shared: {}", describe_shared_holders(&shared))
}

fn describe_shared_holders(holders: &[FileLockHolderInfo]) -> String {
    let mut described = holders
        .iter()
        .take(MAX_NAMED_SHARED_HOLDERS)
        .map(|holder| {
            format!(
                "pid {} since {} ({})",
                holder.pid, holder.acquired_at, holder.label
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    if holders.len() > MAX_NAMED_SHARED_HOLDERS {
        described.push_str(&format!(
            "; {} more",
            holders.len() - MAX_NAMED_SHARED_HOLDERS
        ));
    }
    described
}

fn warn_for_contention(lock_path: &Path, label: &str, elapsed: Duration) {
    let holder = describe_file_lock_holders(lock_path);
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
