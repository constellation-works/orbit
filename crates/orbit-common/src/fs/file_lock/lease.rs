use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fs2::FileExt;

use crate::fs::io::{create_new_private_file, create_private_dir_all};

#[cfg(doc)]
use super::probe_file_lock;

/// A file whose name says some work is in progress and whose exclusive lock
/// says the process doing it is still alive.
///
/// [`probe_file_lock`] tells them apart: `Held` while its owner runs, `Free`
/// once the owner died with the work unfinished, since the kernel drops a
/// dead process's locks but not its files. Releasing unnames the lease
/// before unlocking it, so a finished owner never leaves a free lease behind.
/// Dropping one without releasing it leaves exactly that: a dead lease,
/// which is how a failed owner hands its work to recovery.
///
/// The name is not made durable. A dead process leaves it behind, but a host
/// crash may not, so work that must survive one keeps its own durable record.
#[must_use = "a lease dropped without `release` is left behind as a dead lease"]
#[derive(Debug)]
pub struct LeaseFile {
    path: PathBuf,
    file: File,
}

static LEASE_STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl LeaseFile {
    /// Name a lease at `path`, locked by this process, creating the parent
    /// directory if it is missing. A lease already named at `path` is
    /// replaced, so the caller must keep other owners of `path` out.
    pub fn acquire(path: &Path) -> io::Result<Self> {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("lease path '{}' has no parent directory", path.display()),
            ));
        };
        if !parent.try_exists()? {
            create_private_dir_all(parent)?;
        }
        // Lock the file before it carries the lease's name, so no probe can
        // ever find this owner's lease free.
        let staged = parent.join(format!(
            ".{}.{}-{}.staged",
            name.to_string_lossy(),
            std::process::id(),
            LEASE_STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let file = create_new_private_file(&staged)?;
        let named =
            FileExt::try_lock_exclusive(&file).and_then(|()| std::fs::rename(&staged, path));
        if let Err(error) = named {
            let _ = std::fs::remove_file(&staged);
            return Err(error);
        }
        Ok(Self {
            path: path.to_path_buf(),
            file,
        })
    }

    /// Where this lease is named.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Unname the lease, then unlock it.
    pub fn release(self) -> io::Result<()> {
        let removed = match std::fs::remove_file(&self.path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
        drop(self.file);
        removed
    }
}
