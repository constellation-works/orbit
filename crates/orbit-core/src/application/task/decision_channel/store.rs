use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::{
    FileLockOptions, ReportScopedFileLockGuard, acquire_exclusive_file_lock_with_report_scope,
    create_private_dir_all, open_read_only_no_follow, write_new_private_text,
};
use serde::{Deserialize, Serialize};

use crate::OrbitRuntime;
use crate::runtime::plugin::paths::decision_channel_store_dir;
use crate::runtime::plugin::sandbox_mask::{decision_channel_store_masked, not_visible};

use super::registration::ChannelRegistration;
use super::token;

const STORE_FILE: &str = "channels.json";

const STORE_SCHEMA_VERSION: u32 = 1;

#[derive(Default, Serialize, Deserialize)]
pub(super) struct ChannelStoreFile {
    schema_version: u32,
    #[serde(default)]
    pub(super) channels: BTreeMap<String, ChannelRegistration>,
}

impl OrbitRuntime {
    fn decision_channel_dir(&self) -> PathBuf {
        decision_channel_store_dir(&self.global_root())
    }

    /// Inside an agent sandbox the store is masked; reading the sentinel as
    /// an empty store would turn every channel into "not registered".
    fn refuse_masked_decision_channel_store(&self) -> Result<(), OrbitError> {
        if decision_channel_store_masked(&self.global_root()) {
            return Err(not_visible("the decision channel store"));
        }
        Ok(())
    }

    pub(super) fn decision_channel_store(&self) -> Result<ChannelStoreFile, OrbitError> {
        self.refuse_masked_decision_channel_store()?;
        let dir = self.decision_channel_dir();
        if !require_private_store_dir(&dir)? {
            return Ok(empty_store());
        }
        let path = dir.join(STORE_FILE);
        let body = match read_private_store_file(&path) {
            Ok(body) => body,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(empty_store());
            }
            // A plugin child's sandbox denies the store's contents.
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                return Err(not_visible("the decision channel store"));
            }
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {
                return Err(unsafe_store(&path, &error.to_string()));
            }
            Err(error) => {
                return Err(OrbitError::Io(format!("read {}: {error}", path.display())));
            }
        };
        serde_json::from_str(&body).map_err(|error| {
            OrbitError::Execution(format!(
                "decision channel store at {} is unreadable: {error}",
                path.display()
            ))
        })
    }

    pub(super) fn lock_decision_channel_store(
        &self,
    ) -> Result<ReportScopedFileLockGuard, OrbitError> {
        self.refuse_masked_decision_channel_store()?;
        let dir = self.decision_channel_dir();
        require_private_store_dir(&dir)?;
        let lock_path = dir.join("channels.lock");
        acquire_exclusive_file_lock_with_report_scope(
            &lock_path,
            "decision channels",
            FileLockOptions::default(),
        )
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::PermissionDenied => not_visible("the decision channel store"),
            _ => OrbitError::Io(format!("lock {}: {error}", lock_path.display())),
        })
    }

    /// Callers hold `lock_decision_channel_store` across their whole
    /// read-modify-write; this writer does not take it again.
    pub(super) fn write_decision_channel_store(
        &self,
        store: &ChannelStoreFile,
    ) -> Result<(), OrbitError> {
        let dir = self.decision_channel_dir();
        create_private_dir_all(&dir)
            .map_err(|error| OrbitError::Io(format!("create {}: {error}", dir.display())))?;
        let path = dir.join(STORE_FILE);
        let body = serde_json::to_string(store).map_err(|error| {
            OrbitError::Execution(format!("serialize decision channels: {error}"))
        })?;
        let staged = dir.join(format!(".{STORE_FILE}.{}.tmp", token::fresh_key_hex()?));
        let result =
            write_new_private_text(&staged, &body).and_then(|()| std::fs::rename(&staged, &path));
        if let Err(error) = result {
            let _ = std::fs::remove_file(&staged);
            return Err(OrbitError::Io(format!("write {}: {error}", path.display())));
        }
        Ok(())
    }
}

fn empty_store() -> ChannelStoreFile {
    ChannelStoreFile {
        schema_version: STORE_SCHEMA_VERSION,
        channels: BTreeMap::new(),
    }
}

/// The store holds channel keys, so an existing path that is not the running
/// user's private state is refused before any key is parsed. The reason names
/// the path and the metadata fault, never file contents.
fn unsafe_store(path: &Path, why: &str) -> OrbitError {
    OrbitError::PolicyDenied(format!(
        "refusing the decision channel store: {} {why}",
        path.display()
    ))
}

/// Refuse a store directory that is a link, not a directory, owned by another
/// user or open to group or world. `Ok(false)` means it does not exist yet.
#[cfg(unix)]
fn require_private_store_dir(dir: &Path) -> Result<bool, OrbitError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let metadata = match std::fs::symlink_metadata(dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err(not_visible("the decision channel store"));
        }
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "inspect {}: {error}",
                dir.display()
            )));
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(unsafe_store(dir, "is a symlink"));
    }
    if !metadata.is_dir() {
        return Err(unsafe_store(dir, "is not a directory"));
    }
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(unsafe_store(dir, "is owned by another user"));
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(unsafe_store(
            dir,
            &format!("has mode {mode:03o}; it must be 0700"),
        ));
    }
    Ok(true)
}

#[cfg(not(unix))]
fn require_private_store_dir(dir: &Path) -> Result<bool, OrbitError> {
    Ok(dir.exists())
}

/// Open the state file without following a link, check the opened handle's
/// type, owner and mode, and only then read it. Unsafe metadata comes back as
/// `InvalidInput` carrying the reason.
fn read_private_store_file(path: &Path) -> std::io::Result<String> {
    let invalid = |why: String| std::io::Error::new(std::io::ErrorKind::InvalidInput, why);
    let linked = std::fs::symlink_metadata(path)?;
    if linked.file_type().is_symlink() {
        return Err(invalid("is a symlink".to_string()));
    }
    if !linked.is_file() {
        return Err(invalid("is not a regular file".to_string()));
    }
    let mut file = open_read_only_no_follow(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(invalid("is not a regular file".to_string()));
        }
        // SAFETY: `geteuid` has no preconditions and cannot fail.
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(invalid("is owned by another user".to_string()));
        }
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(invalid(format!("has mode {mode:03o}; it must be 0600")));
        }
    }
    let mut body = String::new();
    file.read_to_string(&mut body)?;
    Ok(body)
}
