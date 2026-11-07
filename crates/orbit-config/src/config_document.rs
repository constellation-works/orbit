//! Pure TOML path access for config documents, and the typed read of one
//! dotted path that admission and layering share.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_home_dir;
use serde::de::DeserializeOwned;

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
