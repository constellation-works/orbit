//! Exclusive waiters stop new shared admissions while existing ones drain.
//!
//! Each waiter publishes its own locked file, so announcing intent cannot
//! itself be starved by shared admissions. The admission lock remains the
//! authority; these records only give its exclusive waiters priority.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use fs2::FileExt;

use super::paths::validated_generation_root;
use super::refusal::refusal;
use super::registry::collect_abandoned_stage;
use crate::OrbitError;

const WAITERS_DIR: &str = ".generation-admission-waiters";

/// A published exclusive waiter. Drop withdraws it, including on timeout.
pub(super) struct ExclusiveWaiter {
    file: File,
    path: PathBuf,
}

impl Drop for ExclusiveWaiter {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
        let _ = std::fs::remove_file(&self.path);
    }
}

impl ExclusiveWaiter {
    /// Publish intent before waiting for exclusive admission. A participant
    /// that cannot write under the root retains the lock-only protocol.
    pub(super) fn publish(root: &Path) -> Result<Option<Self>, OrbitError> {
        let root = validated_generation_root(root)?;
        let dir = root.join(WAITERS_DIR);
        if !dir.starts_with(&root) {
            return Err(refusal("admission waiter directory escapes the root"));
        }
        match crate::fs::io::create_private_dir_all(&dir) {
            Ok(()) => {}
            Err(error) if read_only(&error) => return Ok(None),
            Err(error) => return Err(refusal(error)),
        }
        if dir.canonicalize().map_err(refusal)? != dir {
            return Err(refusal("admission waiter directory must not be a symlink"));
        }
        // A crash releases its lock; the next exclusive waiter collects the
        // abandoned record. Shared observers never need write permission.
        let _ = pending(&root, true)?;
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).map_err(refusal)?;
        let name = format!(
            "{}-{}",
            std::process::id(),
            nonce.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        let staged = dir.join(format!("{name}.staged"));
        let path = dir.join(format!("{name}.waiting"));
        if !staged.starts_with(&dir) || !path.starts_with(&dir) {
            return Err(refusal("admission waiter record escapes its directory"));
        }
        let file = match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&staged)
        {
            Ok(file) => file,
            Err(error) if read_only(&error) => return Ok(None),
            Err(error) => return Err(refusal(error)),
        };
        let mut waiter = Self { file, path: staged };
        FileExt::try_lock_exclusive(&waiter.file).map_err(refusal)?;
        // Publish only after locking: a collector cannot remove a record
        // between its creation and its owner's first lock.
        std::fs::rename(&waiter.path, &path).map_err(refusal)?;
        waiter.path = path;
        Ok(Some(waiter))
    }
}

fn read_only(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
    )
}

/// Whether a live exclusive waiter has published intent. Its OS lock, not
/// its PID or an expiring lease, defines liveness.
pub(super) fn pending(root: &Path, collect: bool) -> Result<bool, OrbitError> {
    let root = validated_generation_root(root)?;
    let dir = root.join(WAITERS_DIR);
    if !dir.starts_with(&root) {
        return Err(refusal("admission waiter directory escapes the root"));
    }
    match dir.canonicalize() {
        Ok(canonical) if canonical != dir => {
            return Err(refusal("admission waiter directory must not be a symlink"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(refusal(error)),
    }
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(refusal(error)),
    };
    let mut live = false;
    for entry in entries {
        let entry = entry.map_err(refusal)?;
        let path = entry.path();
        if !path.starts_with(&dir) {
            continue;
        }
        let kind = match entry.file_type() {
            Ok(kind) => kind,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(refusal(error)),
        };
        if !kind.is_file() {
            continue;
        }
        match path.extension().and_then(|extension| extension.to_str()) {
            Some("waiting") => {}
            Some("staged") if collect => {
                collect_abandoned_stage(&path);
                continue;
            }
            _ => continue,
        }
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(refusal(error)),
        };
        match FileExt::try_lock_shared(&file) {
            Ok(()) => {
                let _ = FileExt::unlock(&file);
                if collect {
                    let _ = std::fs::remove_file(&path);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => live = true,
            Err(error) => return Err(refusal(error)),
        }
    }
    Ok(live)
}
