//! Retired and deprecated `config.toml` keys that warn on every load.
//!
//! Diagnosis lists each key with the file that still sets it. The repair
//! copies that file, then removes only those keys through
//! [`ConfigStore::unset_value`].

use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_home_dir;
use orbit_config::{
    CompatibilityConfigKeyDisposition, ConfigScope, ConfigStore, compatibility_config_keys_at,
    compatibility_config_keys_present,
};
use orbit_core::OrbitRuntime;

use super::{WorkspaceDoctorResult, WorkspaceDoctorStatus, actionable_check, check};

const CHECK: &str = "retired-config-keys";
const FIX: &str = "Run `orbit doctor --fix-retired-config-keys`.";

struct ConfigLayer {
    path: PathBuf,
    scope: ConfigScope,
}

/// One key the repair deleted, and the backup of the file it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedRetiredConfigKey {
    /// Registry key that was removed.
    pub key: String,
    /// Config file the key was removed from.
    pub config_path: PathBuf,
    /// Copy of that file taken before the removal.
    pub backup_path: PathBuf,
}

/// Outcome of `--fix-retired-config-keys`.
///
/// An empty `removed` list means both config files were already free of
/// retired and deprecated keys, and nothing was written.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RetiredConfigKeyRepair {
    /// Keys removed, in file order then registry order.
    pub removed: Vec<RemovedRetiredConfigKey>,
}

pub(super) fn doctor_check_retired_config_keys(
    runtime: &OrbitRuntime,
) -> Vec<WorkspaceDoctorResult> {
    let mut warnings = Vec::new();
    for layer in config_layers(runtime) {
        match compatibility_config_keys_at(&layer.path) {
            Ok(keys) => {
                for key in keys {
                    warnings.push(actionable_check(
                        CHECK,
                        WorkspaceDoctorStatus::Warning,
                        format!(
                            "{} is set in {} ({})",
                            key.key,
                            layer.path.display(),
                            disposition_label(key.disposition)
                        ),
                        FIX.to_string(),
                    ));
                }
            }
            Err(error) => warnings.push(check(
                CHECK,
                WorkspaceDoctorStatus::Warning,
                format!("cannot read {}: {error}", layer.path.display()),
            )),
        }
    }
    if warnings.is_empty() {
        vec![check(
            CHECK,
            WorkspaceDoctorStatus::Ok,
            "no retired or deprecated config keys".to_string(),
        )]
    } else {
        warnings
    }
}

pub(super) fn remove_retired_config_keys(
    runtime: &OrbitRuntime,
) -> Result<RetiredConfigKeyRepair, OrbitError> {
    let mut removed = Vec::new();
    for layer in config_layers(runtime) {
        if compatibility_config_keys_at(&layer.path)?.is_empty() {
            continue;
        }
        removed.extend(repair_layer(runtime, &layer)?);
    }
    Ok(RetiredConfigKeyRepair { removed })
}

fn repair_layer(
    runtime: &OrbitRuntime,
    layer: &ConfigLayer,
) -> Result<Vec<RemovedRetiredConfigKey>, OrbitError> {
    ConfigStore::with_write_lock(&layer.path, || {
        let original = fs::read_to_string(&layer.path).map_err(|error| {
            OrbitError::Io(format!(
                "failed to read config '{}': {error}",
                redact_home_dir(&layer.path.display().to_string())
            ))
        })?;
        let document = toml::from_str(&original).map_err(|error| {
            OrbitError::InvalidInput(format!(
                "invalid runtime config '{}': {error}",
                redact_home_dir(&layer.path.display().to_string())
            ))
        })?;
        let keys = compatibility_config_keys_present(&document);
        let mut store = ConfigStore::open(layer.scope, &layer.path)?;
        let mut removed_keys = Vec::new();
        for key in keys {
            if store.unset_value(key.key)? {
                removed_keys.push(key.key.to_string());
            }
        }
        if removed_keys.is_empty() {
            return Ok(Vec::new());
        }
        validate_edited(runtime, layer.scope, &store)?;
        let backup_path = allocate_backup(&layer.path)?;
        fs::copy(&layer.path, &backup_path).map_err(|error| {
            OrbitError::Io(format!(
                "failed to back up config '{}' to '{}': {error}",
                redact_home_dir(&layer.path.display().to_string()),
                redact_home_dir(&backup_path.display().to_string())
            ))
        })?;
        store.save()?;
        Ok(removed_keys
            .into_iter()
            .map(|key| RemovedRetiredConfigKey {
                key,
                config_path: layer.path.clone(),
                backup_path: backup_path.clone(),
            })
            .collect())
    })
}

fn validate_edited(
    runtime: &OrbitRuntime,
    scope: ConfigScope,
    store: &ConfigStore,
) -> Result<(), OrbitError> {
    match scope {
        ConfigScope::Global => {
            store.validate()?;
            store.validate_global_with_workspace(&runtime.shared_root())
        }
        ConfigScope::Workspace => store.validate_workspace_with_global(&runtime.global_root()),
    }
}

fn config_layers(runtime: &OrbitRuntime) -> Vec<ConfigLayer> {
    let global = runtime.global_root().join("config.toml");
    let workspace = runtime.shared_root().join("config.toml");
    let mut layers = vec![ConfigLayer {
        path: global.clone(),
        scope: ConfigScope::Global,
    }];
    if workspace != global {
        layers.push(ConfigLayer {
            path: workspace,
            scope: ConfigScope::Workspace,
        });
    }
    layers
}

fn disposition_label(disposition: CompatibilityConfigKeyDisposition) -> &'static str {
    match disposition {
        CompatibilityConfigKeyDisposition::Removed => "removed and ignored",
        CompatibilityConfigKeyDisposition::Deprecated => "deprecated",
    }
}

/// `config.toml.bak`, or `config.toml.bak.N` when that name is already taken.
fn allocate_backup(config: &Path) -> Result<PathBuf, OrbitError> {
    let parent = config.parent().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "config path has no parent directory: {}",
            redact_home_dir(&config.display().to_string())
        ))
    })?;
    let name = config.file_name().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "config path has no file name: {}",
            redact_home_dir(&config.display().to_string())
        ))
    })?;
    let name = name.to_string_lossy();
    let candidates = std::iter::once(parent.join(format!("{name}.bak")))
        .chain((1..1000).map(|index| parent.join(format!("{name}.bak.{index}"))));
    for candidate in candidates {
        if candidate.symlink_metadata().is_err() {
            return Ok(candidate);
        }
    }
    Err(OrbitError::InvalidInput(format!(
        "could not choose a backup name beside {}",
        redact_home_dir(&config.display().to_string())
    )))
}
