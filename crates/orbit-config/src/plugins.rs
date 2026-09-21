//! `[plugins.<ns>]`: dynamically named configuration sections owned by
//! installed plugins (design `docs/design/plugins/1_scope.md` §1).
//!
//! The shape mirrors `[crews.<name>]`: the keys are not known at compile time,
//! so they are not registry rows, but a live `plugins.<ns>.<key>` is
//! addressable by `orbit config get`/`set` and layers workspace-over-global
//! like any other table.
//!
//! What *is* declared lives in the plugin's own JSON Schema, which this crate
//! cannot read: plugin installs are host-local state owned by `orbit-core`.
//! Core registers the schemas of the plugins a runtime loaded, and admission
//! consults that registry. A process that never opened a runtime therefore
//! admits a well-formed `plugins.<ns>.<key>` structurally — it has no basis to
//! claim the key is misspelled — while refusing to *fail* on an unknown
//! section, which is what keeps a workspace that pins a plugin this host has
//! not installed loadable (§3).

use std::collections::BTreeMap;
use std::sync::{OnceLock, RwLock};

use orbit_common::OrbitError;
use serde_json::Value as JsonValue;

/// Dotted prefix of the plugin configuration table.
pub const PLUGIN_CONFIG_PREFIX: &str = "plugins";

/// One installed plugin's configuration contract.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PluginConfigSchema {
    /// The plugin's namespace, which is also its config table name.
    pub namespace: String,
    /// `spec.config.schema`, when the manifest declares one.
    pub schema: Option<JsonValue>,
    /// `spec.config.defaults`, applied under any configured value.
    pub defaults: BTreeMap<String, JsonValue>,
}

impl PluginConfigSchema {
    /// Keys the schema declares, in schema order. Empty when the plugin ships
    /// no schema or an open one, which means no key can be called undeclared.
    pub fn declared_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .schema
            .as_ref()
            .and_then(|schema| schema.get("properties"))
            .and_then(JsonValue::as_object)
            .map(|properties| properties.keys().cloned().collect())
            .unwrap_or_default();
        for key in self.defaults.keys() {
            if !keys.contains(key) {
                keys.push(key.clone());
            }
        }
        keys.sort();
        keys
    }

    /// Configured values layered over the manifest defaults.
    pub fn with_defaults(&self, configured: Option<&JsonValue>) -> JsonValue {
        let mut merged = serde_json::Map::new();
        for (key, value) in &self.defaults {
            merged.insert(key.clone(), value.clone());
        }
        if let Some(configured) = configured.and_then(JsonValue::as_object) {
            for (key, value) in configured {
                merged.insert(key.clone(), value.clone());
            }
        }
        JsonValue::Object(merged)
    }

    /// Check one `[plugins.<ns>]` section against the plugin's schema.
    ///
    /// The message names the offending key, because that is the edit the
    /// operator has to make.
    pub fn validate_section(&self, section: &JsonValue) -> Result<(), String> {
        let Some(schema) = &self.schema else {
            return Ok(());
        };
        let compiled = jsonschema::JSONSchema::compile(schema).map_err(|error| {
            format!(
                "plugin '{}' declares a config schema that does not compile: {error}",
                self.namespace
            )
        })?;
        let Err(errors) = compiled.validate(section) else {
            return Ok(());
        };
        let details = errors
            .map(|error| {
                let pointer = error.instance_path.to_string();
                let key = pointer.trim_start_matches('/').replace('/', ".");
                if key.is_empty() {
                    format!("[plugins.{}]: {error}", self.namespace)
                } else {
                    format!("plugins.{}.{key}: {error}", self.namespace)
                }
            })
            .collect::<Vec<_>>();
        Err(details.join("; "))
    }
}

fn registry() -> &'static RwLock<BTreeMap<String, PluginConfigSchema>> {
    static REGISTRY: OnceLock<RwLock<BTreeMap<String, PluginConfigSchema>>> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(BTreeMap::new()))
}

/// Publish the config contracts of the plugins a runtime loaded.
///
/// Registration is additive per namespace: a process may hold several
/// runtimes at once (the scheduler tick opens one per registered workspace),
/// and one of them loading no plugins must not erase what another published.
/// Re-registering a namespace replaces its contract, which is how an upgraded
/// plugin's schema takes effect.
pub fn register_plugin_config_schemas(schemas: Vec<PluginConfigSchema>) {
    if schemas.is_empty() {
        return;
    }
    if let Ok(mut registry) = registry().write() {
        for schema in schemas {
            registry.insert(schema.namespace.clone(), schema);
        }
    }
}

/// The contract registered for one namespace, if any.
pub fn plugin_config_schema(namespace: &str) -> Option<PluginConfigSchema> {
    registry()
        .read()
        .ok()
        .and_then(|registry| registry.get(namespace).cloned())
}

/// Every registered namespace, sorted.
pub fn registered_plugin_namespaces() -> Vec<String> {
    registry()
        .read()
        .map(|registry| registry.keys().cloned().collect())
        .unwrap_or_default()
}

/// One live field on a plugin section, as `orbit config get`/`set` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginFieldKey<'a> {
    /// Namespace between `plugins.` and the key.
    pub namespace: &'a str,
    /// The key the plugin's schema declares.
    pub key: &'a str,
}

/// Parse `plugins.<ns>.<key>` when `key` is a plugin-table path.
///
/// `None` means this is not a plugin key (including the bare `plugins`
/// table). An ill-formed path is an error rather than a fallthrough to the
/// fixed-key registry, so `plugins.graph` is not reported as an unknown
/// setting when what it needs is a field.
pub fn parse_plugin_field_key(key: &str) -> Result<Option<PluginFieldKey<'_>>, OrbitError> {
    let mut parts = key.split('.');
    if parts.next() != Some(PLUGIN_CONFIG_PREFIX) {
        return Ok(None);
    }
    let Some(namespace) = parts.next() else {
        return Ok(None);
    };
    let Some(field) = parts.next() else {
        return Err(OrbitError::InvalidInput(format!(
            "plugin config keys are plugins.<ns>.<key>; '{key}' is missing a key"
        )));
    };
    if parts.next().is_some() {
        return Err(OrbitError::InvalidInput(format!(
            "plugin config keys are plugins.<ns>.<key>; '{key}' has extra segments"
        )));
    }
    if namespace.is_empty() || field.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "plugin config keys are plugins.<ns>.<key>; '{key}' has an empty segment"
        )));
    }
    Ok(Some(PluginFieldKey {
        namespace,
        key: field,
    }))
}

/// Admit one `plugins.<ns>.<key>` against the registered contracts.
///
/// With no plugin loaded there is nothing to check the key against, so it is
/// admitted: refusing here would make every plugin key unsettable on a host
/// that has not opened a runtime. With plugins loaded, an unknown namespace
/// and an undeclared key are both refused, each with the names that would
/// have worked.
pub fn admit_plugin_field_key(parsed: PluginFieldKey<'_>) -> Result<(), OrbitError> {
    let namespaces = registered_plugin_namespaces();
    if namespaces.is_empty() {
        return Ok(());
    }
    let Some(schema) = plugin_config_schema(parsed.namespace) else {
        return Err(OrbitError::invalid_input_with_suggestions(
            format!(
                "no plugin named '{}' is installed on this host, so it owns no \
                 [plugins.{}] section",
                parsed.namespace, parsed.namespace
            ),
            namespaces
                .iter()
                .map(|namespace| format!("plugins.{namespace}"))
                .collect(),
        ));
    };
    let declared = schema.declared_keys();
    if declared.is_empty() || declared.iter().any(|key| key == parsed.key) {
        return Ok(());
    }
    Err(OrbitError::invalid_input_with_suggestions(
        format!(
            "plugin '{}' declares no config key '{}'",
            parsed.namespace, parsed.key
        ),
        declared
            .iter()
            .map(|key| format!("plugins.{}.{key}", parsed.namespace))
            .collect(),
    ))
}

/// Check every `[plugins.<ns>]` section in a loaded document against the
/// registered contracts. Sections for a namespace this host has not installed
/// are left alone: the runtime must still build (§3).
pub fn validate_plugin_sections(sections: &BTreeMap<String, JsonValue>) -> Result<(), OrbitError> {
    for (namespace, section) in sections {
        let Some(schema) = plugin_config_schema(namespace) else {
            continue;
        };
        schema
            .validate_section(&schema.with_defaults(Some(section)))
            .map_err(OrbitError::InvalidInput)?;
    }
    Ok(())
}
