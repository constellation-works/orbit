//! `[plugins.<ns>]`: the bridge between the config crate, which admits the
//! section structurally, and the plugin that owns its schema (§1).
//!
//! `orbit-config` cannot read installed plugins — they are host-local state
//! this crate owns — so this module publishes each loaded plugin's contract to
//! the config crate and applies it where the installed set is known: at plugin
//! load, which is the only point that can refuse *that plugin* and leave the
//! runtime standing (§4.9).

use std::collections::BTreeMap;

use orbit_config::{PluginConfigSchema, register_plugin_config_schemas};
use orbit_tools::plugin::LoadedPlugin;
use serde_json::Value;

/// The config contract one loaded plugin declares.
pub fn plugin_config_schema(plugin: &LoadedPlugin) -> PluginConfigSchema {
    PluginConfigSchema {
        namespace: plugin.namespace().to_string(),
        schema: plugin.config_schema.clone(),
        defaults: plugin.config_defaults.clone().into_iter().collect(),
    }
}

/// Check one plugin's `[plugins.<ns>]` section against its schema.
///
/// Defaults are applied first, so a schema that requires a key the manifest
/// defaults is satisfied without the operator restating it. The message names
/// the offending key.
pub fn validate_plugin_config(
    plugin: &LoadedPlugin,
    sections: &BTreeMap<String, Value>,
) -> Result<(), String> {
    let schema = plugin_config_schema(plugin);
    let configured = sections.get(plugin.namespace());
    schema.validate_section(&schema.with_defaults(configured))
}

/// The values a plugin's templates and backend see: configured over declared
/// defaults, rendered as strings for `{{config.<key>}}`.
pub fn plugin_config_values(
    plugin: &LoadedPlugin,
    sections: &BTreeMap<String, Value>,
) -> BTreeMap<String, String> {
    let schema = plugin_config_schema(plugin);
    schema
        .with_defaults(sections.get(plugin.namespace()))
        .as_object()
        .map(|values| {
            values
                .iter()
                .map(|(key, value)| {
                    let rendered = match value {
                        Value::String(text) => text.clone(),
                        other => other.to_string(),
                    };
                    (key.clone(), rendered)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Publish the loaded plugins' contracts so `orbit config get`/`set` can tell
/// a declared key from a typo, and warn about every section that belongs to a
/// plugin this host has not installed.
///
/// An unknown section is a warning and never a failure: a workspace shared
/// across machines carries the sections of every plugin any of them uses, and
/// the runtime has to build on all of them (§3).
pub fn publish_plugin_config_contracts(
    plugins: &[&LoadedPlugin],
    sections: &BTreeMap<String, Value>,
) {
    let schemas: Vec<PluginConfigSchema> = plugins
        .iter()
        .map(|plugin| plugin_config_schema(plugin))
        .collect();
    for namespace in sections.keys() {
        if schemas.iter().any(|schema| &schema.namespace == namespace) {
            continue;
        }
        tracing::warn!(
            target: "orbit.core.plugin",
            plugin = %namespace,
            "config declares [plugins.{namespace}] but no plugin named '{namespace}' is \
             installed and enabled on this host; the section is ignored — install it with \
             `orbit plugin add`, or remove the section",
        );
    }
    register_plugin_config_schemas(schemas);
}
