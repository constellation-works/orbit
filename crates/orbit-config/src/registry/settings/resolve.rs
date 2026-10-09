use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::observability::log_rotation::LogRotationConfig;
use orbit_common::security::redaction::redact_home_dir;
use orbit_types::identity::{
    Crew, resolve_crew, validate_machine_id, validate_machine_name, validate_stored_task_prefix,
};
use orbit_types::workflow::Provider;
use orbit_types::workflow::automation::members::MaterialField;
use serde::de::DeserializeOwned;

use super::super::{
    CONSTELLATION_DEFAULT_PROVIDER_ENV, DEFAULT_WORKFLOW_CREW, LEGACY_DEFAULT_WORKFLOW_CREW,
};
use crate::memory_limit::MemoryLimit;

/// Built-in `workflow.final_recovery_crews`: the strongest reasoning crews, so
/// the last automated look at a failed task is the most capable one.
pub(crate) const DEFAULT_FINAL_RECOVERY_CREWS: &[&str] = &["sol:100", "opus:20"];

pub(crate) fn read_optional<T: DeserializeOwned>(
    document: &toml::Value,
    key: &str,
    config_path: &Path,
) -> Result<Option<T>, OrbitError> {
    let mut value = document;
    for segment in key.split('.') {
        let table = value.as_table().ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "invalid runtime config '{}': table path for '{key}' contains a non-table value",
                redact_home_dir(&config_path.display().to_string())
            ))
        })?;
        let Some(next) = table.get(segment) else {
            return Ok(None);
        };
        value = next;
    }
    value.clone().try_into().map(Some).map_err(|error| {
        // `orbit config set` falls back to a plain string when the value is not
        // a TOML literal, so `A,B` for a list key lands here as a string.
        let error = error.to_string();
        let error = error.trim_end();
        let hint = if value.is_str() && error.contains("expected a sequence") {
            "; a list is a TOML array, e.g. '[\"A\",\"B\"]' (quote it in the shell)"
        } else {
            ""
        };
        OrbitError::InvalidInput(format!(
            "invalid runtime config '{}': invalid value for '{key}': {error}{hint}",
            redact_home_dir(&config_path.display().to_string())
        ))
    })
}

/// Trim forge logins, drop blanks and duplicates, and keep a stable order.
/// Forge logins are case-insensitive, so they are compared lowercased.
pub(super) fn normalize_logins(raw: Vec<String>) -> Vec<String> {
    let mut logins = raw
        .iter()
        .map(|login| login.trim().to_ascii_lowercase())
        .filter(|login| !login.is_empty())
        .collect::<Vec<_>>();
    logins.sort();
    logins.dedup();
    logins
}

/// Default `execution.proc_spawn_max_timeout_minutes`: long enough for a
/// cold workspace build and test gate, still bounded below a typical activity.
pub(super) const DEFAULT_PROC_SPAWN_MAX_TIMEOUT_MINUTES: u32 = 45;

/// Default `retention.audit_days` and `retention.runs_days`.
const DEFAULT_RETENTION_DAYS: u32 = 60;

pub(super) fn resolve_retention_days(raw: Option<u32>, key: &str) -> Result<u32, OrbitError> {
    const MAX_DAYS: u32 = 36_500;
    match raw {
        Some(value) if value == 0 || value > MAX_DAYS => Err(OrbitError::InvalidInput(format!(
            "{key} has invalid value {value}; expected 1..={MAX_DAYS}"
        ))),
        Some(value) => Ok(value),
        None => Ok(DEFAULT_RETENTION_DAYS),
    }
}

/// Admit a positive minute budget, defaulting when unset. A day is the
/// ceiling: anything longer is indistinguishable from never escalating.
pub(super) fn resolve_bounded_minutes(
    raw: Option<u32>,
    default: u32,
    key: &str,
) -> Result<u32, OrbitError> {
    const MAX_MINUTES: u32 = 1440;
    match raw {
        Some(value) if value == 0 || value > MAX_MINUTES => Err(OrbitError::InvalidInput(format!(
            "{key} has invalid value {value}; expected 1..={MAX_MINUTES}"
        ))),
        Some(value) => Ok(value),
        None => Ok(default),
    }
}

/// Default `machine.worker_memory_high`: throttle one run well before it can
/// crowd out the host (2026-09-23 OOM outage, ORB-12903).
pub(super) const DEFAULT_WORKER_MEMORY_HIGH: MemoryLimit = MemoryLimit::Percent(40);
/// Default `machine.worker_memory_max`: one runaway run keeps at most half of
/// physical RAM, leaving the rest for the host and sibling runs.
pub(super) const DEFAULT_WORKER_MEMORY_MAX: MemoryLimit = MemoryLimit::Percent(50);
const DEFAULT_WORKER_TASKS_MAX: u32 = 4096;

/// Admit a systemd memory size through [`MemoryLimit::parse`].
///
/// The value later becomes one `systemd-run --property=` argument, so
/// anything outside the grammar is refused here instead of failing every
/// worker launch.
pub(super) fn resolve_memory_limit(
    raw: Option<String>,
    default: MemoryLimit,
    key: &str,
) -> Result<MemoryLimit, OrbitError> {
    let Some(value) = raw else {
        return Ok(default);
    };
    MemoryLimit::parse(&value).ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "{key} has invalid value '{}'; expected a size such as 8G or 512M, \
             a percentage of physical memory such as 50%, or infinity",
            value.trim()
        ))
    })
}

pub(super) fn resolve_worker_tasks_max(raw: Option<u32>) -> Result<u32, OrbitError> {
    match raw {
        Some(0) => Err(OrbitError::InvalidInput(
            "machine.worker_tasks_max has invalid value 0; expected >= 1".to_string(),
        )),
        Some(value) => Ok(value),
        None => Ok(DEFAULT_WORKER_TASKS_MAX),
    }
}

pub(super) fn resolve_machine_id(raw: Option<String>) -> Result<Option<String>, OrbitError> {
    let Some(value) = resolve_optional_non_empty(raw, "machine.id")? else {
        return Ok(None);
    };
    validate_machine_id(&value)
        .map_err(|error| OrbitError::InvalidInput(format!("machine.id is invalid: {error}")))?;
    Ok(Some(value))
}

pub(super) fn resolve_machine_name(raw: Option<String>) -> Result<Option<String>, OrbitError> {
    let Some(value) = resolve_optional_non_empty(raw, "machine.name")? else {
        return Ok(None);
    };
    validate_machine_name(&value)
        .map_err(|error| OrbitError::InvalidInput(format!("machine.name is invalid: {error}")))?;
    Ok(Some(value))
}

pub(super) fn resolve_task_prefix(raw: Option<String>) -> Result<Option<String>, OrbitError> {
    let Some(value) = resolve_optional_non_empty(raw, "machine.task_prefix")? else {
        return Ok(None);
    };
    validate_stored_task_prefix(&value).map_err(|error| {
        OrbitError::InvalidInput(format!("machine.task_prefix is invalid: {error}"))
    })?;
    Ok(Some(value))
}

pub(super) fn resolve_choice(
    raw: Option<String>,
    default: &str,
    key: &str,
    choices: &[&str],
) -> Result<String, OrbitError> {
    let value = raw.as_deref().unwrap_or(default).trim();
    if choices.contains(&value) {
        Ok(value.to_string())
    } else {
        Err(OrbitError::InvalidInput(format!(
            "{key} has invalid value '{value}'; expected one of: {}",
            choices.join(", ")
        )))
    }
}

pub(super) fn resolve_optional_choice(
    raw: Option<String>,
    key: &str,
    choices: &[&str],
) -> Result<Option<String>, OrbitError> {
    raw.map(|value| resolve_choice(Some(value), "", key, choices))
        .transpose()
}

pub(super) fn resolve_non_empty(
    raw: Option<String>,
    default: &str,
    key: &str,
) -> Result<String, OrbitError> {
    let value = raw.as_deref().unwrap_or(default).trim();
    if value.is_empty() {
        Err(OrbitError::InvalidInput(format!("{key} must not be empty")))
    } else {
        Ok(value.to_string())
    }
}

/// The default material set when unset; an explicit list must name a field.
pub(super) fn resolve_material_fields(
    raw: Option<Vec<MaterialField>>,
) -> Result<Vec<MaterialField>, OrbitError> {
    match raw {
        None => Ok(MaterialField::DEFAULT.to_vec()),
        Some(fields) if fields.is_empty() => Err(OrbitError::InvalidInput(
            "workflow.task_pilot_freshness.material_fields must name at least one field"
                .to_string(),
        )),
        Some(fields) => Ok(fields),
    }
}

/// `review` unless the owner opts into landing claimed handoffs itself.
pub(super) fn resolve_distributed_completion(raw: Option<String>) -> Result<String, OrbitError> {
    match raw.as_deref().map(str::trim) {
        None | Some("review") => Ok("review".to_string()),
        Some("done") => Ok("done".to_string()),
        Some(other) => Err(OrbitError::InvalidInput(format!(
            "workflow.distributed_completion must be `review` or `done`, not `{other}`"
        ))),
    }
}

pub(super) fn resolve_optional_non_empty(
    raw: Option<String>,
    key: &str,
) -> Result<Option<String>, OrbitError> {
    raw.map(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            Err(OrbitError::InvalidInput(format!("{key} must not be empty")))
        } else {
            Ok(trimmed.to_string())
        }
    })
    .transpose()
}

pub(super) fn resolve_default_crew(
    configured: Option<String>,
    crews: &BTreeMap<String, Crew>,
    env_default: Option<&str>,
    require_selection: bool,
) -> Result<Option<String>, OrbitError> {
    let selected = if let Some(configured) = configured.filter(|value| !value.trim().is_empty()) {
        Some(configured)
    } else if let Some(raw_env) = env_default.filter(|value| !value.trim().is_empty()) {
        let provider = Provider::parse(raw_env).map_err(|error| {
            OrbitError::InvalidInput(format!(
                "{CONSTELLATION_DEFAULT_PROVIDER_ENV} has invalid value: {error}"
            ))
        })?;
        let preferred = match provider.as_str() {
            "claude" => "opus",
            "codex" => "sol",
            provider => provider,
        };
        Some(if crews.contains_key(preferred) {
            preferred.to_string()
        } else {
            provider.as_str().to_string()
        })
    } else {
        None
    };
    if let Some(selected) = selected {
        resolve_crew(&selected, crews)?;
        return Ok(Some(selected));
    }
    if crews.contains_key(DEFAULT_WORKFLOW_CREW) {
        return Ok(Some(DEFAULT_WORKFLOW_CREW.to_string()));
    }
    if crews.contains_key(LEGACY_DEFAULT_WORKFLOW_CREW) {
        return Ok(Some(LEGACY_DEFAULT_WORKFLOW_CREW.to_string()));
    }
    if crews.is_empty() || !require_selection {
        return Ok(None);
    }
    Err(OrbitError::InvalidInput(format!(
        "[workflow].default_crew must be set when defining [crews.*]; choose one of: {}",
        crews.keys().cloned().collect::<Vec<_>>().join(", ")
    )))
}

pub(super) fn default_log_rotation() -> LogRotationConfig {
    LogRotationConfig::default()
}

pub(super) fn default_pass_list() -> Vec<String> {
    #[allow(unused_mut)]
    let mut vars = vec!["HOME", "PATH", "CODEX_HOME", "TMPDIR", "USER"];
    #[cfg(target_os = "macos")]
    vars.push("__CF_USER_TEXT_ENCODING");
    vars.into_iter().map(ToString::to_string).collect()
}

pub(super) fn normalize_pass_list(pass: Vec<String>) -> Result<Vec<String>, OrbitError> {
    let mut normalized = BTreeSet::new();
    for entry in pass {
        let value = entry.trim();
        let mut chars = value.chars();
        let valid = chars
            .next()
            .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
            && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric());
        if !valid {
            return Err(OrbitError::InvalidInput(format!(
                "execution.env.pass contains invalid variable name '{value}'"
            )));
        }
        normalized.insert(value.to_string());
    }
    Ok(normalized.into_iter().collect())
}

pub(super) fn resolve_validation_path_mode(raw: Option<String>) -> Result<String, OrbitError> {
    let mode = raw.unwrap_or_else(|| "prepend".to_string());
    match mode.trim() {
        "prepend" | "replace" => Ok(mode.trim().to_string()),
        other => Err(OrbitError::InvalidInput(format!(
            "workflow.validation_env.path_mode must be `prepend` or `replace`, got `{other}`"
        ))),
    }
}

/// Ceiling for `workflow.resource_throttle.cpu_light_leaves`: a reservation,
/// not a second concurrency limit.
const MAX_CPU_LIGHT_LEAVES: u8 = 32;

pub(super) fn resolve_cpu_light_leaves(raw: Option<u8>) -> Result<u8, OrbitError> {
    let value = raw.unwrap_or(2);
    if value <= MAX_CPU_LIGHT_LEAVES {
        Ok(value)
    } else {
        Err(OrbitError::InvalidInput(format!(
            "workflow.resource_throttle.cpu_light_leaves must be in 0..={MAX_CPU_LIGHT_LEAVES}"
        )))
    }
}

pub(super) fn resolve_percent(raw: Option<u8>, default: u8, key: &str) -> Result<u8, OrbitError> {
    let value = raw.unwrap_or(default);
    if (1..=100).contains(&value) {
        Ok(value)
    } else {
        Err(OrbitError::InvalidInput(format!(
            "{key} must be in 1..=100"
        )))
    }
}
