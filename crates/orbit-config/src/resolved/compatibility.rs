//! Retired configuration keys, migration refusals and compatibility warnings.

use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_home_dir;
use orbit_types::workflow::activity_job::{RETIRED_BACKEND_MIGRATION, check_retired_backend_value};

use crate::layering::value_at_path;
use crate::raw::RawTaskSection;
use crate::registry;

/// The retired invocation-level agent backend override.
pub(crate) const RETIRED_BACKEND_ENV: &str = "ORBIT_BACKEND";

/// [ORB-10801] `ORBIT_BACKEND` and `[runtime] backend` were tiers 2 and 3 of
/// the retired agent-loop backend precedence chain. Both are refused rather
/// than ignored: an operator who still pins `http` must be told their runs are
/// now CLI-agent runs instead of having that substitution made for them.
/// `cli` named the surviving path, so it stays accepted and inert.
pub(super) fn reject_retired_backend_overrides(
    document: &toml::Value,
    env_value: Option<&str>,
) -> Result<(), OrbitError> {
    if let Some(raw) = env_value.map(str::trim).filter(|value| !value.is_empty()) {
        check_retired_backend_value(raw).map_err(|error| {
            OrbitError::InvalidInput(format!("{RETIRED_BACKEND_ENV} is retired: {error}"))
        })?;
    }
    let Some(value) = value_at_path(document, "runtime.backend") else {
        return Ok(());
    };
    let raw = value.as_str().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "[runtime] backend must be a string; {RETIRED_BACKEND_MIGRATION}"
        ))
    })?;
    check_retired_backend_value(raw)
        .map_err(|error| OrbitError::InvalidInput(format!("[runtime] {error}")))
}

pub(super) fn reject_stale_agent_tables(
    raw: Option<&BTreeMap<String, toml::Value>>,
) -> Result<(), OrbitError> {
    if raw.is_some() {
        // ORB-00058: source provenance for retiring the old agent-role schema.
        return Err(OrbitError::InvalidInput(
            "config schema no longer supports [agent.<role>] tables; migrate to [crews.<name>] with [workflow].default_crew".to_string(),
        ));
    }
    Ok(())
}
/// [ORB-10801] `[crews.<name>] backend` selected the agent execution backend.
/// Only the CLI agent path survives, so `cli` stays accepted and inert while
/// the removed values are refused: remapping `http` onto the CLI agent would
/// change which runtime the crew dispatches to without saying so.
pub(super) fn reject_retired_crew_backend(crew: &str, raw: Option<&str>) -> Result<(), OrbitError> {
    let Some(value) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    check_retired_backend_value(value)
        .map_err(|error| OrbitError::InvalidInput(format!("[crews.{crew}] {error}")))
}

pub(super) fn validate_task_artifact_store_from_raw(
    raw: Option<&RawTaskSection>,
) -> Result<(), OrbitError> {
    let Some(value) = raw.and_then(|section| section.artifact_store.as_deref()) else {
        return Ok(());
    };
    let trimmed = value.trim();
    Err(OrbitError::InvalidInput(format!(
        "[task] artifact_store is no longer supported; remove the key because v2 task artifacts are always enabled (found '{trimmed}')"
    )))
}

fn warn_deprecated_task_id_pattern(config_path: &Path) {
    let path = redact_home_dir(&config_path.display().to_string());
    tracing::warn!(
        config = %path,
        "knowledge.task_id_pattern is deprecated and ignored",
    );
}

pub(super) struct CompatibilityKeys {
    pub(super) deprecated_task_id_pattern: bool,
    pub(super) retired_duel: bool,
    pub(super) retired_routines: bool,
    pub(super) retired_docs: bool,
    /// Fixed keys from [`registry::REMOVED_CONFIG_KEYS`] the document still
    /// sets, with their migration notes.
    pub(super) removed_keys: Vec<(&'static str, &'static str)>,
    /// Keys from [`registry::DEPRECATED_CONFIG_KEYS`] the document still
    /// sets: translated on load, with their deprecation notes.
    pub(super) deprecated_keys: Vec<(&'static str, &'static str)>,
}

impl CompatibilityKeys {
    pub(super) fn warn(&self, config_path: &Path) {
        if self.deprecated_task_id_pattern {
            warn_deprecated_task_id_pattern(config_path);
        }
        if self.retired_duel {
            warn_retired_duel_config(config_path);
        }
        if self.retired_routines {
            warn_retired_routines_config(config_path);
        }
        if self.retired_docs {
            warn_retired_docs_config(config_path);
        }
        for (key, note) in &self.removed_keys {
            warn_removed_key(config_path, key, note);
        }
        for (key, note) in &self.deprecated_keys {
            warn_deprecated_key(config_path, key, note);
        }
    }
}

pub(crate) fn warn_compatibility_keys(document: &toml::Value, config_path: &Path) {
    CompatibilityKeys {
        deprecated_task_id_pattern: value_at_path(document, "knowledge.task_id_pattern").is_some(),
        retired_duel: value_at_path(document, "duel").is_some(),
        retired_routines: value_at_path(document, "routines").is_some(),
        retired_docs: value_at_path(document, "docs").is_some(),
        removed_keys: removed_keys_present(document),
        deprecated_keys: deprecated_keys_present(document),
    }
    .warn(config_path);
}

pub(super) fn removed_keys_present(document: &toml::Value) -> Vec<(&'static str, &'static str)> {
    keys_present(registry::REMOVED_CONFIG_KEYS, document)
}

pub(super) fn deprecated_keys_present(document: &toml::Value) -> Vec<(&'static str, &'static str)> {
    keys_present(registry::DEPRECATED_CONFIG_KEYS, document)
}

fn keys_present(
    keys: &[(&'static str, &'static str)],
    document: &toml::Value,
) -> Vec<(&'static str, &'static str)> {
    keys.iter()
        .copied()
        .filter(|(key, _)| value_at_path(document, key).is_some())
        .collect()
}

/// [ORB-12723] A key retired from the registry is accepted and ignored for
/// one release so an existing `config.toml` keeps loading; delete the
/// [`registry::REMOVED_CONFIG_KEYS`] entry after that, when the key becomes
/// an ordinary unknown setting.
pub(crate) const REMOVED_CONFIG_KEY_WARNING: &str =
    "config key is removed and ignored; delete it from config.toml";

fn warn_removed_key(config_path: &Path, key: &str, note: &str) {
    let path = redact_home_dir(&config_path.display().to_string());
    tracing::warn!(
        config = %path,
        key,
        note,
        REMOVED_CONFIG_KEY_WARNING,
    );
}

/// [ORB-13992] A deprecated key is still honoured — translated into its
/// replacement when the document is parsed — and warned on every load. A
/// later release makes it an error; delete its
/// [`registry::DEPRECATED_CONFIG_KEYS`] entry and translation then.
pub(crate) const DEPRECATED_CONFIG_KEY_WARNING: &str = "config key is deprecated and translated \
     on load; move it to its replacement — a later release makes it an error";

fn warn_deprecated_key(config_path: &Path, key: &str, note: &str) {
    let path = redact_home_dir(&config_path.display().to_string());
    tracing::warn!(
        config = %path,
        key,
        note,
        DEPRECATED_CONFIG_KEY_WARNING,
    );
}

pub(crate) const RETIRED_DOCS_CONFIG_WARNING: &str =
    "[docs] is removed and ignored; delete the table from config.toml";

fn warn_retired_docs_config(config_path: &Path) {
    let path = redact_home_dir(&config_path.display().to_string());
    tracing::warn!(
        config = %path,
        RETIRED_DOCS_CONFIG_WARNING,
    );
}

pub(crate) const RETIRED_DUEL_CONFIG_WARNING: &str =
    "[duel] and [duel.models] are retired and ignored; remove both keys from config.toml";

fn warn_retired_duel_config(config_path: &Path) {
    let path = redact_home_dir(&config_path.display().to_string());
    tracing::warn!(
        config = %path,
        RETIRED_DUEL_CONFIG_WARNING,
    );
}

/// [ORB-12236] Registering an owner checkout is the automation opt-in, so the
/// versioned `[routines] role` key no longer selects anything. Accepted and
/// ignored for one release; delete this guard after 2026-12-01, when the key
/// becomes an ordinary unknown section.
pub(crate) const RETIRED_ROUTINES_CONFIG_WARNING: &str = "[routines] is retired and ignored; every registered owner checkout is a routine source — \
     remove the section from config.toml";

fn warn_retired_routines_config(config_path: &Path) {
    let path = redact_home_dir(&config_path.display().to_string());
    tracing::warn!(
        config = %path,
        RETIRED_ROUTINES_CONFIG_WARNING,
    );
}
