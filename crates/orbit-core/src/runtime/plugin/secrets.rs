//! Host-owned plugin secrets: the values behind a manifest's `spec.secrets`
//! (design `docs/design/plugins/1_scope.md` §3, "Plugin secrets").
//!
//! One file per plugin, `<global_root>/state/plugin-secrets/<ns>.json`, mode
//! `0600` in a `0700` directory. The tree is on the plugin sandbox's
//! unreadable list ([`orbit_tools::plugin::PLUGIN_SECRET_STORE_DIR`]) with
//! nothing granted back, so no plugin child reads it — the host reads a value
//! and hands it over. There is no OS keychain backend: one file store keeps the
//! same semantics on every host, and compare-and-swap needs a single lock
//! around read-modify-write that a keychain API does not offer.
//!
//! Every write replaces the file with a rename, so a reader never sees half a
//! write, and holds the plugin's lock file for the read-modify-write, so two
//! writers cannot both win a compare-and-swap. Each write stamps a fresh
//! random `version`: opaque, never reused, so a caller holding a version from
//! before a `rm` and `set` cannot mistake the new value for the one it read.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::{
    FileLockGuard, FileLockOptions, acquire_exclusive_file_lock, create_private_dir_all,
    open_read_only_no_follow, write_new_private_text,
};
use orbit_types::plugin::{is_valid_namespace, is_valid_secret_name};
use serde::{Deserialize, Serialize};

use super::paths::plugin_secret_store_dir;

const SECRET_FILE_SCHEMA_VERSION: u32 = 1;

/// Upper bound on one value. Generous for any token or key file, and small
/// enough that a mistaken pipe of a whole log is refused rather than stored.
pub const MAX_PLUGIN_SECRET_BYTES: usize = 64 * 1024;

/// A secret value. `Debug` never prints it, so a value cannot reach a log line
/// through a formatted struct; the one way out is [`Self::expose`].
#[derive(Clone, PartialEq, Eq)]
pub struct PluginSecretValue(String);

impl PluginSecretValue {
    /// Accept a value: non-empty, at most [`MAX_PLUGIN_SECRET_BYTES`], no NUL.
    /// The refusal never quotes the value.
    pub fn new(value: String) -> Result<Self, OrbitError> {
        if value.is_empty() {
            return Err(OrbitError::InvalidInput(
                "a secret value must not be empty".to_string(),
            ));
        }
        if value.len() > MAX_PLUGIN_SECRET_BYTES {
            return Err(OrbitError::InvalidInput(format!(
                "a secret value may be at most {MAX_PLUGIN_SECRET_BYTES} bytes"
            )));
        }
        if value.contains('\0') {
            return Err(OrbitError::InvalidInput(
                "a secret value must not contain a NUL byte".to_string(),
            ));
        }
        Ok(Self(value))
    }

    /// The value itself, for the one caller that delivers it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PluginSecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PluginSecretValue(<redacted>)")
    }
}

/// One stored secret with the version it was written at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSecret {
    pub value: PluginSecretValue,
    pub version: String,
    pub updated_at: String,
}

/// What may be shown about a stored secret: everything but the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSecretMetadata {
    pub name: String,
    pub version: String,
    pub updated_at: String,
}

/// The outcome of [`PluginSecretStore::compare_and_swap`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginSecretSwap {
    /// The expectation held and the new value is stored at `version`.
    Applied { version: String },
    /// Another write got there first; nothing was stored. `current` is the
    /// version now stored, or `None` when the secret is unset.
    Stale { current: Option<String> },
}

#[derive(Default, Serialize, Deserialize)]
struct SecretFile {
    schema_version: u32,
    plugin: String,
    #[serde(default)]
    secrets: BTreeMap<String, StoredSecret>,
}

/// Deliberately not `Debug`: this is the one struct that holds a raw value.
#[derive(Clone, Serialize, Deserialize)]
struct StoredSecret {
    value: String,
    version: String,
    updated_at: String,
}

impl StoredSecret {
    fn into_secret(self) -> PluginSecret {
        PluginSecret {
            value: PluginSecretValue(self.value),
            version: self.version,
            updated_at: self.updated_at,
        }
    }
}

/// The host's secret store under one global root.
#[derive(Debug, Clone)]
pub struct PluginSecretStore {
    dir: PathBuf,
}

impl PluginSecretStore {
    pub fn new(global_root: &Path) -> Self {
        Self {
            dir: plugin_secret_store_dir(global_root),
        }
    }

    /// The stored value of `plugin`'s secret `name`, with its version.
    pub fn get(&self, plugin: &str, name: &str) -> Result<Option<PluginSecret>, OrbitError> {
        let mut file = self.read(plugin)?;
        Ok(file.secrets.remove(name).map(StoredSecret::into_secret))
    }

    /// Every stored secret of `plugin`, by name, without values.
    pub fn list(&self, plugin: &str) -> Result<Vec<PluginSecretMetadata>, OrbitError> {
        Ok(self
            .read(plugin)?
            .secrets
            .into_iter()
            .map(|(name, secret)| PluginSecretMetadata {
                name,
                version: secret.version,
                updated_at: secret.updated_at,
            })
            .collect())
    }

    /// Store `value` whatever is there now; the operator's `set`. Returns the
    /// new version.
    pub fn put(
        &self,
        plugin: &str,
        name: &str,
        value: &PluginSecretValue,
    ) -> Result<String, OrbitError> {
        match self.swap(plugin, name, value, None)? {
            PluginSecretSwap::Applied { version } => Ok(version),
            PluginSecretSwap::Stale { .. } => Err(OrbitError::Execution(
                "an unconditional secret write reported a stale version".to_string(),
            )),
        }
    }

    /// Store `value` only if the secret is still at `expected_version`
    /// (`None`: only if it is unset). The check and the write happen under
    /// the plugin's lock, so of two writers expecting one version exactly one
    /// is applied.
    pub fn compare_and_swap(
        &self,
        plugin: &str,
        name: &str,
        value: &PluginSecretValue,
        expected_version: Option<&str>,
    ) -> Result<PluginSecretSwap, OrbitError> {
        self.swap(plugin, name, value, Some(expected_version))
    }

    /// Delete one secret. `Ok(false)` when it was not set.
    pub fn remove(&self, plugin: &str, name: &str) -> Result<bool, OrbitError> {
        let _lock = self.lock(plugin)?;
        let mut file = self.read(plugin)?;
        let removed = file.secrets.remove(name).is_some();
        if removed {
            self.write(plugin, file)?;
        }
        Ok(removed)
    }

    /// Delete every secret `plugin` has, returning their names.
    pub fn remove_all(&self, plugin: &str) -> Result<Vec<String>, OrbitError> {
        self.retain(plugin, |_| false)
    }

    /// Keep only the secrets `keep` accepts, returning the names removed.
    pub fn retain(
        &self,
        plugin: &str,
        keep: impl Fn(&str) -> bool,
    ) -> Result<Vec<String>, OrbitError> {
        let _lock = self.lock(plugin)?;
        let mut file = self.read(plugin)?;
        let removed: Vec<String> = file
            .secrets
            .keys()
            .filter(|name| !keep(name))
            .cloned()
            .collect();
        if removed.is_empty() {
            return Ok(removed);
        }
        for name in &removed {
            file.secrets.remove(name);
        }
        self.write(plugin, file)?;
        Ok(removed)
    }

    /// `expected`: `None` writes unconditionally; `Some(v)` requires the
    /// stored version to be `v` (`Some(None)`: requires it unset).
    fn swap(
        &self,
        plugin: &str,
        name: &str,
        value: &PluginSecretValue,
        expected: Option<Option<&str>>,
    ) -> Result<PluginSecretSwap, OrbitError> {
        if !is_valid_secret_name(name) {
            return Err(OrbitError::InvalidInput(format!(
                "'{name}' is not a valid secret name"
            )));
        }
        let _lock = self.lock(plugin)?;
        let mut file = self.read(plugin)?;
        let current = file.secrets.get(name).map(|secret| secret.version.clone());
        if let Some(expected) = expected
            && current.as_deref() != expected
        {
            return Ok(PluginSecretSwap::Stale { current });
        }
        let version = fresh_version()?;
        file.secrets.insert(
            name.to_string(),
            StoredSecret {
                value: value.expose().to_string(),
                version: version.clone(),
                updated_at: chrono::Utc::now().to_rfc3339(),
            },
        );
        self.write(plugin, file)?;
        Ok(PluginSecretSwap::Applied { version })
    }

    fn file_path(&self, plugin: &str) -> Result<PathBuf, OrbitError> {
        if !is_valid_namespace(plugin) {
            return Err(OrbitError::InvalidInput(format!(
                "'{plugin}' is not a valid plugin namespace"
            )));
        }
        Ok(self.dir.join(format!("{plugin}.json")))
    }

    fn lock(&self, plugin: &str) -> Result<FileLockGuard, OrbitError> {
        let path = self.file_path(plugin)?.with_extension("lock");
        self.refuse_symlinked_dir()?;
        acquire_exclusive_file_lock(&path, "plugin secrets", FileLockOptions::default())
            .map_err(|error| OrbitError::Io(format!("lock {}: {error}", path.display())))
    }

    fn read(&self, plugin: &str) -> Result<SecretFile, OrbitError> {
        let path = self.file_path(plugin)?;
        self.refuse_symlinked_dir()?;
        let mut handle = match open_read_only_no_follow(&path) {
            Ok(handle) => handle,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SecretFile {
                    schema_version: SECRET_FILE_SCHEMA_VERSION,
                    plugin: plugin.to_string(),
                    secrets: BTreeMap::new(),
                });
            }
            Err(error) => {
                return Err(OrbitError::Io(format!("open {}: {error}", path.display())));
            }
        };
        let mut raw = String::new();
        handle
            .read_to_string(&mut raw)
            .map_err(|error| OrbitError::Io(format!("read {}: {error}", path.display())))?;
        // A parse error from serde can quote the offending input, which here
        // is a value; report where the file is and nothing from inside it.
        let file: SecretFile = serde_json::from_str(&raw).map_err(|_| {
            OrbitError::Execution(format!(
                "{} is not a readable secret file; remove it and set the plugin's secrets again",
                path.display()
            ))
        })?;
        if file.schema_version != SECRET_FILE_SCHEMA_VERSION || file.plugin != plugin {
            return Err(OrbitError::Execution(format!(
                "{} does not hold schema {SECRET_FILE_SCHEMA_VERSION} secrets for plugin \
                 '{plugin}'",
                path.display()
            )));
        }
        Ok(file)
    }

    /// Replace the plugin's file with `file`, or delete it once it is empty.
    /// Called with the plugin's lock held.
    fn write(&self, plugin: &str, mut file: SecretFile) -> Result<(), OrbitError> {
        let path = self.file_path(plugin)?;
        if file.secrets.is_empty() {
            return match std::fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(OrbitError::Io(format!(
                    "remove {}: {error}",
                    path.display()
                ))),
            };
        }
        file.schema_version = SECRET_FILE_SCHEMA_VERSION;
        file.plugin = plugin.to_string();
        let body = serde_json::to_string(&file)
            .map_err(|error| OrbitError::Execution(format!("serialize plugin secrets: {error}")))?;
        create_private_dir_all(&self.dir)
            .map_err(|error| OrbitError::Io(format!("create {}: {error}", self.dir.display())))?;
        restrict_dir(&self.dir)?;
        let staged = self
            .dir
            .join(format!(".{plugin}.json.{}.tmp", fresh_version()?));
        // An exclusive create at `0600`, then a rename over the old file: a
        // reader sees the previous file or this one, and the value is never
        // on disk under a broader mode.
        let result =
            write_new_private_text(&staged, &body).and_then(|()| std::fs::rename(&staged, &path));
        if let Err(error) = result {
            let _ = std::fs::remove_file(&staged);
            return Err(OrbitError::Io(format!("write {}: {error}", path.display())));
        }
        Ok(())
    }

    /// The store directory is host-owned; a link there would send every value
    /// wherever it points.
    fn refuse_symlinked_dir(&self) -> Result<(), OrbitError> {
        match std::fs::symlink_metadata(&self.dir) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                Err(OrbitError::PolicyDenied(format!(
                    "refusing the plugin secret store at {}: it is not a plain directory",
                    self.dir.display()
                )))
            }
            _ => Ok(()),
        }
    }
}

/// Keep the store directory `0700` even when something created it with a
/// broader mode.
fn restrict_dir(dir: &Path) -> Result<(), OrbitError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| OrbitError::Io(format!("restrict {}: {error}", dir.display())))?;
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// 128 random bits as hex: opaque, and never repeated across writes.
fn fresh_version() -> Result<String, OrbitError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| OrbitError::Execution(format!("draw a secret version: {error}")))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
