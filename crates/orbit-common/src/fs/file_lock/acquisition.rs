use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fs2::FileExt;

use crate::fs::io::{
    classify_lock_io, classify_or_wrap_lock_io, create_private_dir_all, open_private_file,
};

use super::hold::{HoldReport, duration_millis, record_lock_wait};
use super::holders::{
    clear_file_lock_holder, describe_file_lock_holders, describe_queued_exclusive_waiter,
    read_file_lock_holder, read_shared_file_lock_holders, write_file_lock_holder,
    write_shared_holder_record,
};
use super::options::{FileLockOptions, FileLockTimeout};

const FILE_LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(50);

/// First and longest pause between a contended acquisition's attempts. The
/// pause doubles from the first to the longest, so a short hold is followed
/// within a millisecond or two and a waiter never sleeps more than the
/// longest past a release. A flat 50 ms pause left the lock idle for up to
/// 50 ms after every release, and a convoy of queued sections paid that gap
/// once per holder (ORB-15797).
const CONTENDED_RETRY_FIRST: Duration = Duration::from_millis(1);
const CONTENDED_RETRY_LONGEST: Duration = Duration::from_millis(10);

/// How long a single-shot acquisition waits out a refusal that no holder
/// claims — see [`try_acquire_exclusive_file_lock`]. Sized for a forked child
/// to reach `execve` on a loaded host, not for a holder to finish its work.
const UNCLAIMED_LOCK_GRACE: Duration = Duration::from_secs(2);

/// Suffix after the lock file's name that names its turnstile:
/// `<lock>.turnstile`. Dot-prefixed beside a dot-prefixed lock for the same
/// reason as the shared holders' records.
const TURNSTILE_SUFFIX: &str = ".turnstile";

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
    let _timing = AcquisitionTiming(started);
    loop {
        match FileExt::try_lock_exclusive(&lock_file) {
            Ok(()) => {
                drop(_timing);
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

/// What [`probe_file_lock`] found at a lock path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileLockProbe {
    /// Nothing is named there.
    Absent,
    /// Some open file description holds the lock.
    Held,
    /// The file is there and nobody holds its lock.
    Free,
}

/// Whether anyone holds an advisory lock on the file at the exact
/// `lock_path`, without waiting, creating it, or writing holder metadata.
///
/// For a file whose lock is its owner's liveness: an owner that locks it
/// before naming it, and unnames it before unlocking it, is alive exactly
/// while a probe finds it [`FileLockProbe::Held`]. A process that dies
/// releases its locks, so a file still named but [`FileLockProbe::Free`]
/// outlived its owner. A probe that loses a race with that owner unnaming the
/// file reports it absent, not free. A descriptor a forked child inherited
/// holds the lock until the child execs, which only delays `Free`.
pub fn probe_file_lock(lock_path: &Path) -> io::Result<FileLockProbe> {
    // A file renamed over this name since the open is another owner's; look
    // again, a bounded number of times, then call it held.
    for _ in 0..3 {
        let file = match File::open(lock_path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(FileLockProbe::Absent);
            }
            Err(error) => return Err(error),
        };
        match FileExt::try_lock_shared(&file) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return Ok(FileLockProbe::Held);
            }
            Err(error) => return Err(error),
        }
        if same_file_still_named(&file, lock_path)? {
            return Ok(FileLockProbe::Free);
        }
    }
    Ok(FileLockProbe::Held)
}

#[cfg(unix)]
fn same_file_still_named(file: &File, path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let opened = file.metadata()?;
    match std::fs::symlink_metadata(path) {
        Ok(named) => Ok(named.dev() == opened.dev() && named.ino() == opened.ino()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(not(unix))]
fn same_file_still_named(_file: &File, path: &Path) -> io::Result<bool> {
    path.try_exists()
}

pub(crate) fn acquire_shared_file_lock(
    lock_path: &Path,
    label: &str,
    options: FileLockOptions,
) -> io::Result<FileLockGuard> {
    let lock_file = open_lock_file(lock_path, label)?;
    acquire_file_lock(lock_file, lock_path, label, options, false)
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
    let _timing = AcquisitionTiming(Instant::now());
    let mut wait = ContentionWait::new(lock_path, label, options);
    let requested = wait.started;
    let turnstile_path = options
        .prefer_exclusive_waiters
        .then(|| turnstile_path(lock_path))
        .flatten();
    if !exclusive && let Some(turnstile) = &turnstile_path {
        pass_turnstile(turnstile, lock_path, &mut wait)?;
    }
    // Held by this exclusive acquirer from its first refusal until it takes
    // the lock, so readers arriving meanwhile queue behind it.
    let mut queued: Option<FileLockGuard> = None;

    loop {
        let acquisition = if exclusive {
            FileExt::try_lock_exclusive(&lock_file)
        } else {
            FileExt::try_lock_shared(&lock_file)
        };
        match acquisition {
            Ok(()) => {
                drop(queued);
                record_lock_wait(lock_path, requested);
                drop(_timing);
                // Re-entrant nested sections never reach here, so these
                // events count the acquisitions a caller actually paid for.
                crate::tracing::trace!(
                    target: "orbit.common.fs.file_lock",
                    lock_path = %lock_path.display(),
                    mode = if exclusive { "exclusive" } else { "shared" },
                    "advisory file lock acquired",
                );
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
                    hold: options
                        .warn_held_after
                        .map(|warn_after| HoldReport::new(lock_path, label, exclusive, warn_after)),
                });
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if exclusive
                    && queued.is_none()
                    && let Some(turnstile) = &turnstile_path
                {
                    queued = try_hold_turnstile(turnstile, label);
                }
                wait.refused(|| describe_file_lock_holders(lock_path))?;
            }
            Err(error) => {
                return Err(classify_or_wrap_lock_io(lock_path, error, |error| {
                    format!("lock {label} '{}': {error}", lock_path.display())
                }));
            }
        }
    }
}

struct AcquisitionTiming(Instant);

impl Drop for AcquisitionTiming {
    fn drop(&mut self) {
        crate::fs::lock_wait::record(self.0.elapsed());
    }
}

/// One acquisition's deadline, contention warning and retry pacing, shared by
/// its turnstile and lock waits so the two together stay within one deadline.
struct ContentionWait<'a> {
    lock_path: &'a Path,
    label: &'a str,
    options: FileLockOptions,
    started: Instant,
    warned: bool,
    /// The pause before the next attempt; see [`CONTENDED_RETRY_FIRST`].
    retry: Duration,
}

impl<'a> ContentionWait<'a> {
    fn new(lock_path: &'a Path, label: &'a str, options: FileLockOptions) -> Self {
        Self {
            lock_path,
            label,
            options,
            started: Instant::now(),
            warned: false,
            retry: CONTENDED_RETRY_FIRST,
        }
    }

    /// Account for one refusal: time out past the deadline, warn once past
    /// the warning threshold naming `holder()`, otherwise sleep before the
    /// next attempt.
    fn refused(&mut self, holder: impl FnOnce() -> String) -> io::Result<()> {
        let elapsed = self.started.elapsed();
        if elapsed >= self.options.timeout {
            let timeout = FileLockTimeout {
                lock_path: self.lock_path.to_path_buf(),
                label: self.label.to_string(),
                timeout_ms: duration_millis(self.options.timeout),
                holder: read_file_lock_holder(self.lock_path),
                shared_holders: read_shared_file_lock_holders(self.lock_path),
            };
            return Err(io::Error::new(io::ErrorKind::TimedOut, timeout));
        }
        if !self.warned && elapsed >= self.options.warn_after {
            self.warned = true;
            warn_for_contention(self.lock_path, self.label, elapsed, &holder());
        }
        std::thread::sleep(self.retry.min(self.options.timeout - elapsed));
        self.retry = (self.retry * 2).min(CONTENDED_RETRY_LONGEST);
        Ok(())
    }
}

fn turnstile_path(lock_path: &Path) -> Option<PathBuf> {
    let name = lock_path.file_name()?.to_str()?;
    Some(lock_path.with_file_name(format!("{name}{TURNSTILE_SUFFIX}")))
}

/// Wait until no exclusive acquirer is queued at `turnstile`, then pass
/// straight through: the shared hold is dropped before the lock is tried, so
/// readers never hold the turnstile while they wait or work.
///
/// The turnstile only orders acquirers; the lock itself still excludes. So a
/// reader never creates it — a missing turnstile means no writer has queued,
/// and a read must not leave a file behind — and one it cannot open or lock
/// is passed rather than failing the read.
fn pass_turnstile(
    turnstile: &Path,
    lock_path: &Path,
    wait: &mut ContentionWait<'_>,
) -> io::Result<()> {
    let Ok(gate) = crate::fs::open_read_only_no_follow(turnstile) else {
        return Ok(());
    };
    loop {
        match FileExt::try_lock_shared(&gate) {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                wait.refused(|| describe_queued_exclusive_waiter(turnstile, lock_path))?;
            }
            Ok(()) | Err(_) => return Ok(()),
        }
    }
}

/// Queue at `turnstile` if no other exclusive acquirer already holds it.
/// The holder record names this waiter to the readers it holds back. A
/// turnstile that cannot be created or locked leaves the waiter unqueued:
/// it still takes the lock, only without priority.
fn try_hold_turnstile(turnstile: &Path, label: &str) -> Option<FileLockGuard> {
    let gate = open_lock_file(turnstile, label).ok()?;
    FileExt::try_lock_exclusive(&gate).ok()?;
    write_file_lock_holder(&gate, label);
    Some(FileLockGuard {
        file: gate,
        clear_holder_on_drop: true,
        shared_record: None,
        hold: None,
    })
}

fn warn_for_contention(lock_path: &Path, label: &str, elapsed: Duration, holder: &str) {
    crate::tracing::warn!(
        target: "orbit.common.fs.file_lock",
        lock_path = %lock_path.display(),
        label,
        waited_ms = duration_millis(elapsed),
        holder,
        "still waiting for advisory file lock; a holder may be hung",
    );
}
