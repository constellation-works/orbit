//! Plugin root and CLI flag derivation.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{PLUGIN_DIR_NAME, PluginManifestError};

/// The plugin root an authoring command writes into for `dir`: `dir` itself
/// when it is already a [`PLUGIN_DIR_NAME`] directory, `dir/.orbit-plugin`
/// otherwise.
pub fn plugin_root_in(dir: &Path) -> PathBuf {
    if dir.file_name().is_some_and(|name| name == PLUGIN_DIR_NAME) {
        dir.to_path_buf()
    } else {
        dir.join(PLUGIN_DIR_NAME)
    }
}

/// The long option derived for one top-level tool input property.
///
/// This is shared by manifest validation and the CLI adapter so an accepted
/// manifest cannot produce a different spelling at registration time.
pub fn derive_plugin_cli_flag(name: &str, property: &Value) -> String {
    let mut flag = String::new();
    let mut previous_was_lower_or_digit = false;
    for character in name.chars() {
        match character {
            '_' | ' ' | '-' => {
                flag.push('-');
                previous_was_lower_or_digit = false;
            }
            character if character.is_ascii_uppercase() => {
                if previous_was_lower_or_digit {
                    flag.push('-');
                }
                flag.push(character.to_ascii_lowercase());
                previous_was_lower_or_digit = false;
            }
            character if character.is_ascii_lowercase() || character.is_ascii_digit() => {
                flag.push(character);
                previous_was_lower_or_digit = true;
            }
            _ => {}
        }
    }
    if !flag.is_empty() && schema_property_uses_json_flag(property) {
        flag.push_str("-json");
    }
    flag
}

fn schema_property_uses_json_flag(property: &Value) -> bool {
    match property.get("type").and_then(Value::as_str) {
        Some("string" | "integer" | "number" | "boolean") => false,
        Some("array") => !matches!(
            property
                .get("items")
                .and_then(|items| items.get("type"))
                .and_then(Value::as_str),
            Some("string" | "integer" | "number")
        ),
        _ => true,
    }
}

/// Refuse top-level schema properties that would produce an ambiguous or
/// unusable plugin CLI flag.
pub fn validate_plugin_cli_flags(
    input_schema: &Value,
    field: &str,
) -> Result<(), PluginManifestError> {
    let Some(properties) = input_schema.get("properties").and_then(Value::as_object) else {
        return Ok(());
    };

    let mut flags = std::collections::BTreeMap::new();
    for (property_name, property) in properties {
        let flag = derive_plugin_cli_flag(property_name, property);
        let property_field = format!("{field}.properties.{property_name}");
        if flag.is_empty() {
            return Err(PluginManifestError::new(
                property_field,
                format!("property '{property_name}' derives an empty CLI flag"),
            ));
        }
        if let Some(previous_property) = flags.insert(flag.clone(), property_name) {
            return Err(PluginManifestError::new(
                property_field,
                format!(
                    "properties '{previous_property}' and '{property_name}' both derive CLI flag '--{flag}'"
                ),
            ));
        }
    }
    Ok(())
}

/// Refuse a `cli.positional` list that the CLI adapter would silently drop:
/// a repeated entry, or one that does not name a top-level property of the
/// tool's `input_schema`.
///
/// The adapter fills a positional from the property of the same name, so an
/// entry naming nothing gets no argument at all and the documented
/// `orbit <ns> <verb> <value>` form simply never appears. `input_schema` is
/// `None` for a tool that declares none, which has no properties to name.
pub fn validate_plugin_cli_positionals(
    verb: &str,
    input_schema: Option<&Value>,
    positional: &[String],
    field: &str,
) -> Result<(), PluginManifestError> {
    let properties = input_schema
        .and_then(|schema| schema.get("properties"))
        .and_then(Value::as_object);
    let mut seen = std::collections::BTreeSet::new();
    for (index, name) in positional.iter().enumerate() {
        let entry_field = format!("{field}[{index}]");
        if !seen.insert(name.as_str()) {
            return Err(PluginManifestError::new(
                entry_field,
                format!("tool '{verb}' promotes '{name}' to a positional argument twice"),
            ));
        }
        if !properties.is_some_and(|properties| properties.contains_key(name)) {
            return Err(PluginManifestError::new(
                entry_field,
                format!(
                    "tool '{verb}' promotes '{name}' to a positional argument, but its \
                     input_schema declares no top-level property of that name"
                ),
            ));
        }
    }
    Ok(())
}
