//! Serialization between concurrent `orbit update` runs.
//!
//! Two updates racing on one install directory would interleave backup,
//! rename, and migration against the same files. This is a *try*-lock rather
//! than the blocking [`orbit_common::fs::io::with_exclusive_file_lock`]: a
//! second update should say so immediately, not queue behind a download and
//! then apply a version the operator has since stopped wanting.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use orbit_common::OrbitError;

/// Held for the duration of one update; released when dropped.
#[derive(Debug)]
pub struct UpdateLock {
    file: File,
    path: PathBuf,
}

impl UpdateLock {
    /// Take the update lock for `install_dir`, or report who holds it.
    pub fn acquire(install_dir: &Path) -> Result<Self, OrbitError> {
        std::fs::create_dir_all(install_dir).map_err(|error| {
            OrbitError::Io(format!(
                "failed to create install directory '{}': {error}",
                install_dir.display()
            ))
        })?;
        let path = install_dir.join(".orbit-update.lock");
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| {
                OrbitError::Io(format!(
                    "failed to open the update lock '{}': {error}",
                    path.display()
                ))
            })?;
        file.try_lock_exclusive().map_err(|_| {
            OrbitError::Execution(format!(
                "another orbit update is already running for '{}'; wait for it to finish, \
                 then re-run `orbit update` (it is idempotent and will resume)",
                install_dir.display()
            ))
        })?;
        Ok(Self { file, path })
    }
}

impl Drop for UpdateLock {
    fn drop(&mut self) {
        if let Err(error) = FileExt::unlock(&self.file) {
            tracing::debug!(
                lock = %self.path.display(),
                %error,
                "failed to release the orbit update lock"
            );
        }
    }
}
