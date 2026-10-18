//! Retired and deprecated `config.toml` keys that warn on every load.
//!
//! Diagnosis lists each key with the file that still sets it. The repair
//! migrates translated review settings within each file before removing the
//! compatibility keys. Legacy after-landing policies require operator migration;
//! both layers are preflighted under their write locks before any backup or save.

use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_home_dir;
use orbit_config::{
    CompatibilityConfigKeyDisposition, ConfigScope, ConfigStore, REVIEW_BEFORE_PR_KEY,
    REVIEW_MINUTES_KEY, compatibility_config_keys_at, compatibility_config_keys_present,
    translate_legacy_review_keys,
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
    let layers = config_layers(runtime);
    with_config_locks(&layers, || {
        // Plan every layer before writing either one. In particular, a workspace
        // refusal must not leave a global policy half-migrated.
        let mut planned = Vec::new();
        for layer in &layers {
            if let Some(repair) = prepare_layer(runtime, layer)? {
                planned.push(repair);
            }
        }
        let mut removed = Vec::new();
        for mut repair in planned {
            let backup_path = allocate_backup(&repair.layer.path)?;
            fs::copy(&repair.layer.path, &backup_path).map_err(|error| {
                OrbitError::Io(format!(
                    "failed to back up config '{}' to '{}': {error}",
                    redact_home_dir(&repair.layer.path.display().to_string()),
                    redact_home_dir(&backup_path.display().to_string())
                ))
            })?;
            repair.store.save()?;
            removed.extend(
                repair
                    .removed_keys
                    .into_iter()
                    .map(|key| RemovedRetiredConfigKey {
                        key,
                        config_path: repair.layer.path.clone(),
                        backup_path: backup_path.clone(),
                    }),
            );
        }
        Ok(RetiredConfigKeyRepair { removed })
    })
}

// Always acquire global then workspace, keeping the versions we preflight
// stable until the migration finishes. ConfigStore::save takes a reentrant lock.
fn with_config_locks<T>(
    layers: &[ConfigLayer],
    op: impl FnOnce() -> Result<T, OrbitError>,
) -> Result<T, OrbitError> {
    match layers.split_first() {
        Some((layer, rest)) => {
            ConfigStore::with_write_lock(&layer.path, || with_config_locks(rest, op))
        }
        None => op(),
    }
}

struct PlannedRepair<'a> {
    layer: &'a ConfigLayer,
    store: ConfigStore,
    removed_keys: Vec<String>,
}

fn prepare_layer<'a>(
    runtime: &OrbitRuntime,
    layer: &'a ConfigLayer,
) -> Result<Option<PlannedRepair<'a>>, OrbitError> {
    if compatibility_config_keys_at(&layer.path)?.is_empty() {
        return Ok(None);
    }
    let original = fs::read_to_string(&layer.path).map_err(|error| {
        OrbitError::Io(format!(
            "failed to read config '{}': {error}",
            redact_home_dir(&layer.path.display().to_string())
        ))
    })?;
    let mut document: toml::Value = toml::from_str(&original).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "invalid runtime config '{}': {error}",
            redact_home_dir(&layer.path.display().to_string())
        ))
    })?;
    let keys = compatibility_config_keys_present(&document);
    // Even a masked after-landing value requires an explicit operator choice:
    // deleting the masking layer could otherwise change the enabled consumer.
    if document
        .get("operation")
        .and_then(|operation| operation.get("review_policy"))
        .and_then(toml::Value::as_str)
        .is_some_and(|policy| policy.trim() == "after-landing")
    {
        return Err(OrbitError::InvalidInput(format!(
            "cannot automatically remove operation.review_policy = \"after-landing\" from {}: \
             run `orbit auto-task toggle delivery-code-review on` (or `off` if review should \
             stay disabled), set review.before_pr and review.minutes explicitly as needed, \
             then delete operation.review_policy manually and rerun the repair; \
             no config files were changed",
            redact_home_dir(&layer.path.display().to_string())
        )));
    }
    translate_legacy_review_keys(&mut document, &layer.path)?;
    let mut store = ConfigStore::open(layer.scope, &layer.path)?;
    for (key, field) in [
        (REVIEW_BEFORE_PR_KEY, "before_pr"),
        (REVIEW_MINUTES_KEY, "minutes"),
    ] {
        if !store.is_key_set(key)
            && let Some(value) = document.get("review").and_then(|review| review.get(field))
        {
            store.set_value(key, &value.to_string())?;
        }
    }
    let mut removed_keys = Vec::new();
    for key in keys {
        if store.unset_value(key.key)? {
            removed_keys.push(key.key.to_string());
        }
    }
    validate_edited(runtime, layer.scope, &store)?;
    Ok(Some(PlannedRepair {
        layer,
        store,
        removed_keys,
    }))
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
