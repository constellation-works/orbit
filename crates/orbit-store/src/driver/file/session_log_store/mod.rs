//! Directory layout, lock and durable writes of the workspace session log.
//!
//! The log is JSON Lines at `<workspace>/.orbit/session-log.jsonl`. Every read
//! and mutation is serialized through one stable advisory-lock sidecar,
//! `.session-log.jsonl.lock`, so ID allocation, append and resolution observe
//! one ordered record stream. The log must be a regular file, and opening it
//! never follows a final-component symlink. The record format is
//! `repository/session_log.rs`'s.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_bytes, sync_parent_dir};
use orbit_common::fs::open_read_only_no_follow;

use crate::fs::lock::{FileLockGuard, acquire_exclusive};

const LOG_FILE_NAME: &str = "session-log.jsonl";
const LOCK_FILE_NAME: &str = ".session-log.jsonl.lock";

/// One workspace's `.orbit` directory, which holds its session log.
#[derive(Debug, Clone)]
pub struct SessionLogDir {
    orbit_dir: PathBuf,
}

impl SessionLogDir {
    /// The session log of the workspace whose `.orbit` directory is
    /// `orbit_dir`.
    pub fn new(orbit_dir: impl Into<PathBuf>) -> Self {
        Self {
            orbit_dir: orbit_dir.into(),
        }
    }

    /// The log file's path.
    pub(crate) fn log_path(&self) -> PathBuf {
        self.orbit_dir.join(LOG_FILE_NAME)
    }

    /// Hold the session-log lock for `purpose` until the guard drops.
    pub(crate) fn lock(&self, purpose: &str) -> Result<FileLockGuard, OrbitError> {
        acquire_exclusive(&self.orbit_dir.join(LOCK_FILE_NAME), purpose)
    }

    /// The log's bytes; an absent log reads as empty. Refuses a symlink or a
    /// non-regular file.
    pub(crate) fn read(&self) -> Result<Vec<u8>, OrbitError> {
        read_log(&self.log_path())
    }

    /// Append `encoded` durably, creating the log (and syncing its parent)
    /// when it does not exist yet.
    pub(crate) fn append(&self, encoded: &[u8]) -> Result<(), OrbitError> {
        append_bytes(&self.log_path(), encoded)
    }

    /// Cut the log to its first `len` bytes.
    pub(crate) fn truncate(&self, len: usize) -> Result<(), OrbitError> {
        truncate_log(&self.log_path(), len)
    }

    /// Terminate an unterminated final row.
    pub(crate) fn append_newline(&self) -> Result<(), OrbitError> {
        append_trailing_newline(&self.log_path())
    }

    /// Replace the whole log with `encoded` by an atomic rename.
    pub(crate) fn replace(&self, encoded: &[u8]) -> Result<(), OrbitError> {
        let path = self.log_path();
        atomic_write_bytes(&path, encoded)
            .map_err(|error| OrbitError::Io(format!("replace {}: {error}", path.display())))
    }
}

fn read_log(path: &Path) -> Result<Vec<u8>, OrbitError> {
    let mut file = match open_read_only_no_follow(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(OrbitError::Io(format!("open {}: {error}", path.display())));
        }
    };
    check_log_file(
        path,
        &file
            .metadata()
            .map_err(|error| OrbitError::Io(format!("inspect {}: {error}", path.display())))?,
    )?;
    let mut raw = Vec::new();
    file.read_to_end(&mut raw)
        .map_err(|error| OrbitError::Io(format!("read {}: {error}", path.display())))?;
    Ok(raw)
}

fn check_log_file(path: &Path, metadata: &fs::Metadata) -> Result<(), OrbitError> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(OrbitError::Io(format!(
            "session-log path must be a regular file, not a symlink: {}",
            path.display()
        )));
    }
    Ok(())
}

fn open_log_for_write(path: &Path, options: &mut OpenOptions) -> Result<File, OrbitError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => check_log_file(path, &metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "inspect {}: {error}",
                path.display()
            )));
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options
        .open(path)
        .map_err(|error| OrbitError::Io(format!("open {}: {error}", path.display())))?;
    check_log_file(
        path,
        &file
            .metadata()
            .map_err(|error| OrbitError::Io(format!("inspect {}: {error}", path.display())))?,
    )?;
    Ok(file)
}

fn append_bytes(path: &Path, encoded: &[u8]) -> Result<(), OrbitError> {
    let existed = path.exists();
    let mut file = open_log_for_write(path, OpenOptions::new().create(true).append(true))?;
    file.write_all(encoded)
        .map_err(|error| OrbitError::Io(format!("append {}: {error}", path.display())))?;
    orbit_common::fs::io::sync_file_data(&file)
        .map_err(|error| OrbitError::Io(format!("sync {}: {error}", path.display())))?;
    if !existed {
        let parent = path.parent().ok_or_else(|| {
            OrbitError::Io(format!(
                "sync parent for {}: no parent directory",
                path.display()
            ))
        })?;
        let parent_dir = File::open(parent).map_err(|error| {
            OrbitError::Io(format!("sync parent for {}: {error}", path.display()))
        })?;
        sync_parent_dir(&parent_dir).map_err(|error| {
            OrbitError::Io(format!("sync parent for {}: {error}", path.display()))
        })?;
    }
    Ok(())
}

fn truncate_log(path: &Path, truncate_at: usize) -> Result<(), OrbitError> {
    let file = open_log_for_write(path, OpenOptions::new().write(true))?;
    file.set_len(truncate_at as u64)
        .map_err(|error| OrbitError::Io(format!("truncate {}: {error}", path.display())))?;
    orbit_common::fs::io::sync_file(&file)
        .map_err(|error| OrbitError::Io(format!("sync {}: {error}", path.display())))
}

fn append_trailing_newline(path: &Path) -> Result<(), OrbitError> {
    let mut file = open_log_for_write(path, OpenOptions::new().append(true))?;
    file.write_all(b"\n")
        .map_err(|error| OrbitError::Io(format!("append {}: {error}", path.display())))?;
    orbit_common::fs::io::sync_file_data(&file)
        .map_err(|error| OrbitError::Io(format!("sync {}: {error}", path.display())))
}
