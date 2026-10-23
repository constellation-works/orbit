//! Reading, parsing, merging, and editing TOML documents.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_home_dir;

use crate::store::read_config_file;

use super::load::ConfigDocument;

pub(super) fn read_config_document(path: &Path) -> Result<Option<ConfigDocument>, OrbitError> {
    let Some(raw) = read_config_file(path)? else {
        return Ok(None);
    };
    parse_config_document(path, &raw).map(Some)
}

/// Parse one file. Deprecated spellings are not translated here: a
/// workspace file's translation needs the global file beneath it, so the
/// loader translates each document once it knows its layer
/// ([`crate::aliases::translate_deprecated_keys`]).
pub(super) fn parse_config_document(path: &Path, raw: &str) -> Result<ConfigDocument, OrbitError> {
    let value = toml::from_str(raw).map_err(|err| {
        OrbitError::InvalidInput(format!(
            "invalid runtime config '{}': {err}",
            redact_home_dir(&path.display().to_string())
        ))
    })?;
    if let Some(patterns) =
        crate::registry::read_optional::<Vec<String>>(&value, "worktree.reclaim", path)?
    {
        for pattern in patterns {
            orbit_common::fs::path_glob::RelativePathGlob::new(&pattern)
                .map_err(|error| OrbitError::InvalidInput(format!("worktree.reclaim: {error}")))?;
        }
    }
    Ok(ConfigDocument {
        path: path.to_path_buf(),
        value,
    })
}

pub(super) fn empty_document() -> toml::Value {
    toml::Value::Table(toml::map::Map::new())
}

pub(super) fn merge_tables(base: &mut toml::Value, overlay: &toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base), toml::Value::Table(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(key) {
                    Some(existing) => merge_tables(existing, value),
                    None => {
                        base.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (base, overlay) => *base = overlay.clone(),
    }
}

pub(crate) fn value_at_path<'a>(document: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
    let mut value = document;
    for segment in key.split('.') {
        value = value.as_table()?.get(segment)?;
    }
    Some(value)
}

pub(super) fn crew_entry<'a>(
    document: &'a toml::Value,
    name: &str,
) -> Option<&'a toml::map::Map<String, toml::Value>> {
    document
        .as_table()?
        .get("crews")?
        .as_table()?
        .get(name)?
        .as_table()
}

pub(crate) fn set_value_at_path(document: &mut toml::Value, key: &str, value: toml::Value) {
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
            .or_insert_with(empty_document);
        table = entry.as_table_mut();
    }
    if let Some(table) = table {
        table.insert((*last).to_string(), value);
    }
}

pub(super) fn remove_value_at_path(document: &mut toml::Value, key: &str) {
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
