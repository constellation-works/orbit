//! Pure TOML path access for config documents, and the typed read of one
//! dotted path that admission and layering share, and the one constructor for
//! `invalid runtime config '<path>'` errors.

use std::fmt::Display;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_home_dir;
use serde::de::DeserializeOwned;

/// The path with the home directory redacted to `~`, in the form user-facing
/// config errors name it.
pub(crate) fn redacted_display(path: &Path) -> String {
    redact_home_dir(&path.display().to_string())
}

fn invalid_runtime_config_message(path: &Path, detail: impl Display) -> String {
    format!(
        "invalid runtime config '{}': {detail}",
        redacted_display(path)
    )
}

/// `invalid runtime config '<path>': <detail>` as an `InvalidInput` error,
/// naming `path` with the home directory redacted. Every admitted config
/// reader builds this error here.
pub fn invalid_runtime_config(path: &Path, detail: impl Display) -> OrbitError {
    OrbitError::InvalidInput(invalid_runtime_config_message(path, detail))
}

/// [`invalid_runtime_config`] that also carries `did_you_mean` suggestions.
/// Non-empty suggestions yield `OrbitError::InvalidInputDiagnostic`; the
/// message is unchanged.
pub(crate) fn invalid_runtime_config_with_suggestions(
    path: &Path,
    detail: impl Display,
    did_you_mean: Vec<String>,
) -> OrbitError {
    OrbitError::invalid_input_with_suggestions(
        invalid_runtime_config_message(path, detail),
        did_you_mean,
    )
}

pub(crate) fn config_value_at_path<'a>(
    document: &'a toml::Value,
    key: &str,
) -> Option<&'a toml::Value> {
    let mut value = document;
    for segment in key.split('.') {
        value = value.as_table()?.get(segment)?;
    }
    Some(value)
}

pub(crate) fn set_config_value_at_path(document: &mut toml::Value, key: &str, value: toml::Value) {
    let segments = key.split('.').collect::<Vec<_>>();
    let Some((last, ancestors)) = segments.split_last() else {
        return;
    };
    let mut table = document.as_table_mut();
    for segment in ancestors {
        let Some(current) = table else {
            return;
        };
        let entry = current
            .entry((*segment).to_string())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        table = entry.as_table_mut();
    }
    if let Some(table) = table {
        table.insert((*last).to_string(), value);
    }
}

pub(crate) fn remove_config_value_at_path(document: &mut toml::Value, key: &str) {
    let segments = key.split('.').collect::<Vec<_>>();
    let Some((last, ancestors)) = segments.split_last() else {
        return;
    };
    let mut value = document;
    for segment in ancestors {
        let Some(next) = value
            .as_table_mut()
            .and_then(|table| table.get_mut(*segment))
        else {
            return;
        };
        value = next;
    }
    if let Some(table) = value.as_table_mut() {
        table.remove(*last);
    }
}

/// Read the value at a dotted `key` path of a config document and
/// deserialize it as `T`. A missing segment yields `None`; a non-table
/// ancestor or a value that does not deserialize is an `InvalidInput` naming
/// the (home-redacted) `config_path`.
pub(crate) fn read_typed_config_value<T: DeserializeOwned>(
    document: &toml::Value,
    key: &str,
    config_path: &Path,
) -> Result<Option<T>, OrbitError> {
    let mut value = document;
    for segment in key.split('.') {
        let table = value.as_table().ok_or_else(|| {
            invalid_runtime_config(
                config_path,
                format!("table path for '{key}' contains a non-table value"),
            )
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
        invalid_runtime_config(
            config_path,
            format!("invalid value for '{key}': {error}{hint}"),
        )
    })
}
