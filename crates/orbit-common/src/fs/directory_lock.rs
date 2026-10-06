//! Bounded mkdir/mtime locks for peers using the proper-lockfile protocol.

use std::fs::{self, File, FileTimes, Metadata, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{RecvTimeoutError, SyncSender, sync_channel};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use fs2::FileExt;

use super::io::{create_private_dir, create_private_dir_all, open_read_only_no_follow};

/// Timing policy for a proper-lockfile-compatible directory lock.
#[derive(Debug, Clone, Copy)]
pub struct DirectoryLockOptions {
    /// Maximum time spent waiting for another writer.
    pub timeout: Duration,
    /// Age after which an unrefreshed lock may be reclaimed.
    pub stale_after: Duration,
    /// Interval between mtime refreshes while the operation runs.
    pub update_interval: Duration,
}

impl Default for DirectoryLockOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            // proper-lockfile's default stale window is ten seconds. Refresh
            // every second, also respecting its minimum two-second window.
            stale_after: Duration::from_secs(10),
            update_interval: Duration::from_secs(1),
        }
    }
}

/// Run `op` with exclusive ownership of the directory `<target_path>.lock`.
///
/// Acquisition uses atomic mkdir, never flock. The directory's mtime is
/// refreshed while held and the empty directory is removed on return or
/// unwinding. Abandoned regular-file locks from older Orbit versions are
/// removed only after the stale window and an exclusive flock check.
/// This protocol is independent of Orbit's private advisory file locks.
pub fn with_directory_lock<T, E, F>(target_path: &Path, label: &str, op: F) -> Result<T, E>
where
    F: FnOnce() -> Result<T, E>,
    E: From<io::Error>,
{
    with_directory_lock_options(target_path, label, DirectoryLockOptions::default(), op)
}

/// [`with_directory_lock`] with an explicit acquisition and refresh policy.
///
/// The update interval must be positive and at most half the stale window.
/// Like proper-lockfile, stale reclamation relies on timely scheduling and
/// a shared filesystem clock. A compromised lock is reported at release;
/// a synchronous operation already running cannot be interrupted.
pub fn with_directory_lock_options<T, E, F>(
    target_path: &Path,
    label: &str,
    options: DirectoryLockOptions,
    op: F,
) -> Result<T, E>
where
    F: FnOnce() -> Result<T, E>,
    E: From<io::Error>,
{
    if options.update_interval.is_zero() || options.update_interval > options.stale_after / 2 {
        return Err(
            io::Error::new(io::ErrorKind::InvalidInput, "invalid directory lock timing").into(),
        );
    }
    let parent = target_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory lock target has no parent",
        )
    })?;
    let mut name = target_path
        .file_name()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory lock target has no file name",
            )
        })?
        .to_os_string();
    name.push(".lock");
    create_private_dir_all(parent)?;
    let path = fs::canonicalize(parent)?.join(name);
    let mut guard = DirectoryLockGuard::acquire(path, label, options)?;
    let result = op();
    let release = guard.release();
    match result {
        Ok(value) => release.map(|()| value).map_err(E::from),
        Err(error) => {
            if let Err(release_error) = release {
                crate::tracing::warn!(%release_error, label, "failed to release directory lock");
            }
            Err(error)
        }
    }
}

struct DirectoryLockGuard {
    path: PathBuf,
    directory: File,
    stop: SyncSender<()>,
    heartbeat: Option<JoinHandle<io::Result<Metadata>>>,
    released: bool,
}

impl DirectoryLockGuard {
    fn acquire(path: PathBuf, label: &str, options: DirectoryLockOptions) -> io::Result<Self> {
        let started = Instant::now();
        loop {
            let reclaimed = match create_private_dir(&path) {
                Ok(()) => break,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    reclaim_stale_lock(&path, options.stale_after)?
                }
                Err(error) => return Err(error),
            };
            let elapsed = started.elapsed();
            if elapsed >= options.timeout {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "timed out acquiring {label} directory lock '{}'",
                        path.display()
                    ),
                ));
            }
            if !reclaimed {
                thread::sleep(Duration::from_millis(50).min(options.timeout - elapsed));
            }
        }

        let directory = open_directory(&path)?;
        let (stop, receiver) = sync_channel(1);
        let mut guard = Self {
            path,
            directory,
            stop,
            heartbeat: None,
            released: false,
        };
        let held = guard.directory.try_clone()?;
        let heartbeat_path = guard.path.clone();
        let mut expected = held.metadata()?;
        guard.heartbeat = Some(
            thread::Builder::new()
                .name("directory-lock-mtime".into())
                .spawn(move || {
                    loop {
                        match receiver.recv_timeout(options.update_interval) {
                            Ok(()) | Err(RecvTimeoutError::Disconnected) => return Ok(expected),
                            Err(RecvTimeoutError::Timeout) => {
                                verify_owned(&heartbeat_path, &expected)?;
                                held.set_times(FileTimes::new().set_modified(SystemTime::now()))?;
                                expected = held.metadata()?;
                            }
                        }
                    }
                })?,
        );
        Ok(guard)
    }

    fn release(&mut self) -> io::Result<()> {
        self.released = true;
        let _ = self.stop.try_send(());
        let expected = match self.heartbeat.take() {
            Some(heartbeat) => heartbeat
                .join()
                .map_err(|_| io::Error::other("directory lock heartbeat panicked"))??,
            None => self.directory.metadata()?,
        };
        verify_owned(&self.path, &expected)?;
        fs::remove_dir(&self.path)
    }
}

impl Drop for DirectoryLockGuard {
    fn drop(&mut self) {
        if !self.released
            && let Err(error) = self.release()
        {
            crate::tracing::warn!(%error, lock_path = %self.path.display(), "failed to release directory lock");
        }
    }
}

fn verify_owned(path: &Path, expected: &Metadata) -> io::Result<()> {
    let current = fs::symlink_metadata(path)?;
    if !current.is_dir()
        || !same_entry(&current, expected)
        || current.modified()? != expected.modified()?
    {
        return Err(io::Error::other(format!(
            "directory lock compromised: '{}'",
            path.display()
        )));
    }
    Ok(())
}

fn reclaim_stale_lock(path: &Path, stale_after: Duration) -> io::Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error),
    };
    if !metadata.is_dir() && !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "directory lock must be a directory or legacy regular file: '{}'",
                path.display()
            ),
        ));
    }
    if SystemTime::now()
        .duration_since(metadata.modified()?)
        .unwrap_or_default()
        <= stale_after
    {
        return Ok(false);
    }
    // Never unlink an old Orbit lock while an advisory writer still holds it.
    // Open without create/truncate and reject final-component symlinks/FIFOs.
    let legacy = if metadata.is_file() {
        let file = match open_read_only_no_follow(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
            Err(error) => return Err(error),
        };
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) => return Err(error),
        }
        if !same_entry(&metadata, &file.metadata()?) {
            return Ok(false);
        }
        Some(file)
    } else {
        None
    };

    // A heartbeat or a new owner between the first stat and removal cancels
    // reclamation. As in proper-lockfile, stat plus rmdir is not atomic.
    let current = match fs::symlink_metadata(path) {
        Ok(current) => current,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error),
    };
    if !same_entry(&metadata, &current) || metadata.modified()? != current.modified()? {
        return Ok(false);
    }
    let removal = if legacy.is_some() {
        fs::remove_file(path)
    } else {
        fs::remove_dir(path)
    };
    match removal {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error),
    }
}

fn open_directory(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_READ_ATTRIBUTES: u32 = 0x0080;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
        // Open a directory handle, without following a reparse point.
        // SetFileTime needs write-attributes access even on a directory.
        options.access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES);
        options.custom_flags(0x0200_0000 | 0x0020_0000);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "lock is not a directory",
        ));
    }
    Ok(file)
}

fn same_entry(left: &Metadata, right: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        left.dev() == right.dev() && left.ino() == right.ino()
    }
    #[cfg(not(unix))]
    {
        match (left.created(), right.created()) {
            (Ok(left), Ok(right)) => left == right,
            _ => false,
        }
    }
}
