use std::collections::BTreeMap;
use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_common::fs::io::{
    FileLockGuard, FileLockOptions, acquire_exclusive_file_lock, create_private_dir_all,
    write_new_private_text,
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
    pub(super) schema_version: u32,
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
        let path = self.decision_channel_dir().join(STORE_FILE);
        let body = match std::fs::read_to_string(&path) {
            Ok(body) => body,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ChannelStoreFile {
                    schema_version: STORE_SCHEMA_VERSION,
                    channels: BTreeMap::new(),
                });
            }
            // A plugin child's sandbox denies the store's contents.
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                return Err(not_visible("the decision channel store"));
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

    pub(super) fn lock_decision_channel_store(&self) -> Result<FileLockGuard, OrbitError> {
        self.refuse_masked_decision_channel_store()?;
        let lock_path = self.decision_channel_dir().join("channels.lock");
        acquire_exclusive_file_lock(&lock_path, "decision channels", FileLockOptions::default())
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
