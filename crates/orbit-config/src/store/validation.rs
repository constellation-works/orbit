//! Effective and layered config reads and staged-document validation.

use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_home_dir;
use serde_json::Value as JsonValue;

use super::persistence::{self, ConfigScope, ConfigStore};
use crate::ConfigRoots;
use crate::layering::{
    reject_workspace_crew_env, reject_workspace_machine_table, resolve_workspace_file_document,
    validate_staged_global_document, validate_staged_workspace_document,
};
use crate::operation::OperationLayerSource;
use crate::persistence::PersistenceConfig;
use crate::plugin_enablement::reject_global_plugin_enablement;
use crate::registry::{self, ConfigSnapshot};
use crate::resolved::ResolvedConfig;

impl ConfigStore {
    /// The fully resolved (defaulted) view of this document, as if it were
    /// loaded as the effective `config.toml`. [`Self::validate`] uses it to
    /// verify a standalone document before saving. File views use
    /// [`Self::snapshot_with_global`] to admit cross-layer crew references.
    pub fn snapshot(&self) -> Result<ConfigSnapshot, OrbitError> {
        Ok(self.resolved()?.snapshot)
    }

    /// Resolve this file's settings and built-in defaults. For workspace
    /// files, crew definitions layer over the global file so crew references
    /// and partial crew overrides are admitted in their normal context.
    /// Other global settings are excluded; a global snapshot stays isolated.
    pub fn snapshot_with_global(&self, global_root: &Path) -> Result<ConfigSnapshot, OrbitError> {
        // Without a distinct workspace, both runtime roots name the global
        // file. Preserve its standalone semantics, including [machine].
        if self
            .path
            .parent()
            .is_some_and(|root| !ConfigRoots::new(global_root, root).has_workspace_layer())
        {
            return self.snapshot();
        }
        match self.scope {
            ConfigScope::Global => self.snapshot(),
            ConfigScope::Workspace => {
                let global_path = global_root.join("config.toml");
                let global_raw = persistence::read_optional(&global_path)?;
                resolve_workspace_file_document(
                    &global_path,
                    &global_raw,
                    &self.path,
                    &self.doc.to_string(),
                )
                .map(|resolved| resolved.snapshot)
            }
        }
    }

    /// The effective `[plugins.<ns>]` sections of this staged document
    /// layered over the global config at `global_root`, as a runtime load
    /// would resolve them. Lets a caller that stages a workspace edit in
    /// memory validate plugin-owned sections against the staged values
    /// before anything is written.
    pub fn plugin_sections_with_global(
        &self,
        global_root: &Path,
    ) -> Result<BTreeMap<String, JsonValue>, OrbitError> {
        match self.scope {
            ConfigScope::Global => Ok(self.resolved()?.plugins),
            ConfigScope::Workspace => {
                let workspace_root = self.path.parent().ok_or_else(|| {
                    OrbitError::InvalidInput(
                        "workspace config path has no parent directory".to_string(),
                    )
                })?;
                let roots = ConfigRoots::new(global_root, workspace_root);
                Ok(
                    validate_staged_workspace_document(&roots, &self.path, &self.doc.to_string())?
                        .plugins,
                )
            }
        }
    }

    fn resolved(&self) -> Result<ResolvedConfig, OrbitError> {
        // Persistence paths are derived from the two data roots, not from
        // the config document, and are irrelevant to key validation here.
        let persistence =
            PersistenceConfig::default_for_data_root(self.path.parent().unwrap_or(&self.path));
        let operation_layer_source = match self.scope {
            ConfigScope::Global => OperationLayerSource::Global,
            ConfigScope::Workspace => OperationLayerSource::Workspace,
        };
        ResolvedConfig::from_raw_str(
            &self.doc.to_string(),
            &self.path,
            persistence,
            operation_layer_source,
        )
    }

    /// Look up the effective value of a single admitted key.
    pub fn effective_value(&self, key: &str) -> Result<JsonValue, OrbitError> {
        registry::admit_config_key(key)?;
        let resolved = self.resolved()?;
        if let Some(value) = resolved.snapshot.value_for(key) {
            return Ok(value);
        }
        if let Some(value) = plugin_field_value(&resolved, key)? {
            return Ok(value);
        }
        Ok(crew_field_value(&resolved, key)?.unwrap_or(JsonValue::Null))
    }

    /// Run the in-memory document through the exact same
    /// `RawRuntimeConfig` → [`ResolvedConfig`] validation pipeline as
    /// [`ResolvedConfig::load`], without writing anything.
    pub fn validate(&self) -> Result<(), OrbitError> {
        self.reject_workspace_machine_table()?;
        self.snapshot().map(|_| ())
    }

    /// Validate a staged workspace document with the current global config
    /// layered beneath it. Workspace plugin toggles use this before saving so
    /// a pool may refer to a crew defined only in the global file. Workspace
    /// writes use this same layer-aware admission path as runtime loads.
    pub fn validate_workspace_with_global(&self, global_root: &Path) -> Result<(), OrbitError> {
        if self.scope != ConfigScope::Workspace {
            return Err(OrbitError::InvalidInput(
                "layered workspace validation requires a workspace config store".to_string(),
            ));
        }
        let workspace_root = self.path.parent().ok_or_else(|| {
            OrbitError::InvalidInput("workspace config path has no parent directory".to_string())
        })?;
        let roots = ConfigRoots::new(global_root, workspace_root);
        validate_staged_workspace_document(&roots, &self.path, &self.doc.to_string()).map(|_| ())
    }

    /// Validate a staged workspace write and the specific value being set
    /// against the effective config formed by the global and workspace files.
    pub fn validate_workspace_for_set(
        &self,
        key: &str,
        global_root: &Path,
    ) -> Result<(), OrbitError> {
        if self.scope != ConfigScope::Workspace {
            return Err(OrbitError::InvalidInput(
                "layered workspace validation requires a workspace config store".to_string(),
            ));
        }
        let workspace_root = self.path.parent().ok_or_else(|| {
            OrbitError::InvalidInput("workspace config path has no parent directory".to_string())
        })?;
        let roots = ConfigRoots::new(global_root, workspace_root);
        let resolved =
            validate_staged_workspace_document(&roots, &self.path, &self.doc.to_string())?;
        self.validate_set_target(key, &resolved)
    }

    /// A workspace file may not carry `[machine]` at all, and the global file
    /// may not carry `[plugin_enablement]`, however either got there. The
    /// scope-free admission pipeline cannot see which file it is resolving,
    /// so the store — which does — states the rule.
    fn reject_workspace_machine_table(&self) -> Result<(), OrbitError> {
        let document: toml::Value = toml::from_str(&self.doc.to_string()).map_err(|err| {
            OrbitError::InvalidInput(format!(
                "invalid TOML in '{}': {err}",
                redact_home_dir(&self.path.display().to_string())
            ))
        })?;
        match self.scope {
            ConfigScope::Workspace => {
                reject_workspace_machine_table(&document, &self.path)?;
                reject_workspace_crew_env(&document, &self.path)
            }
            ConfigScope::Global => reject_global_plugin_enablement(&document, &self.path),
        }
    }

    /// Admission plus a write-time refusal for `orbit config set`.
    ///
    /// Load ignores an invalid optional crew property so the workspace stays
    /// usable. A deliberate `set` of that same key still fails closed: the
    /// operator asked to persist a value that admission would drop.
    pub fn validate_for_set(&self, key: &str) -> Result<(), OrbitError> {
        self.reject_workspace_machine_table()?;
        let resolved = self.resolved()?;
        self.validate_set_target(key, &resolved)
    }

    /// Validate a staged global document against the workspace layer at
    /// `workspace_root` that it would be read with, through the same layered
    /// load a runtime performs. A global file that is valid alone can still
    /// make that workspace's config unloadable (both review layers on,
    /// resource-throttle resume at or above a workspace high, a crew a
    /// workspace pool names), so the write is refused here instead.
    pub fn validate_global_with_workspace(&self, workspace_root: &Path) -> Result<(), OrbitError> {
        if self.scope != ConfigScope::Global {
            return Err(OrbitError::InvalidInput(
                "layered global validation requires a global config store".to_string(),
            ));
        }
        let global_root = self.path.parent().ok_or_else(|| {
            OrbitError::InvalidInput("global config path has no parent directory".to_string())
        })?;
        let roots = ConfigRoots::new(global_root, workspace_root);
        validate_staged_global_document(&roots, &self.path, &self.doc.to_string())
    }

    /// [`Self::validate_for_set`] for a global edit, plus the layered
    /// admission of [`Self::validate_global_with_workspace`].
    pub fn validate_global_for_set(
        &self,
        key: &str,
        workspace_root: &Path,
    ) -> Result<(), OrbitError> {
        if self.scope != ConfigScope::Global {
            return Err(OrbitError::InvalidInput(
                "layered global validation requires a global config store".to_string(),
            ));
        }
        self.validate_for_set(key)?;
        self.validate_global_with_workspace(workspace_root)
    }

    fn validate_set_target(&self, key: &str, resolved: &ResolvedConfig) -> Result<(), OrbitError> {
        if let Some(ignored) = resolved
            .ignored_crew_properties
            .iter()
            .find(|property| property.config_key() == key)
        {
            return Err(OrbitError::InvalidInput(ignored.error_message.clone()));
        }
        // A `[plugins.<ns>]` value is the plugin's contract, so it is checked
        // against that plugin's own JSON Schema before the write lands. The
        // check is available only once a runtime has published the installed
        // plugins' schemas; without them the write is admitted and the plugin
        // load reports any violation.
        if let Some(parsed) = crate::plugins::parse_plugin_field_key(key)?
            && let Some(schema) = crate::plugins::plugin_config_schema(parsed.namespace)
        {
            let section = resolved.plugins.get(parsed.namespace);
            schema
                .validate_section(&schema.with_defaults(section))
                .map_err(OrbitError::InvalidInput)?;
        }
        Ok(())
    }
}

/// The value one `plugins.<ns>.<key>` holds in this document, falling back to
/// the plugin's declared default when the file does not set it.
fn plugin_field_value(
    resolved: &ResolvedConfig,
    key: &str,
) -> Result<Option<JsonValue>, OrbitError> {
    let Some(parsed) = crate::plugins::parse_plugin_field_key(key)? else {
        return Ok(None);
    };
    if let Some(value) = resolved
        .plugins
        .get(parsed.namespace)
        .and_then(|section| section.get(parsed.key))
    {
        return Ok(Some(value.clone()));
    }
    Ok(Some(
        crate::plugins::plugin_config_schema(parsed.namespace)
            .and_then(|schema| schema.defaults.get(parsed.key).cloned())
            .unwrap_or(JsonValue::Null),
    ))
}

fn crew_field_value(resolved: &ResolvedConfig, key: &str) -> Result<Option<JsonValue>, OrbitError> {
    let Some(parsed) = registry::parse_crew_field_key(key)? else {
        return Ok(None);
    };
    if let Some(human) = resolved.human_crews.get(parsed.name) {
        return Ok(Some(match parsed.field {
            "kind" => serde_json::json!(orbit_types::identity::HUMAN_CREW_KIND),
            "description" => serde_json::json!(human.description),
            "tags" => serde_json::json!(human.tags),
            _ => JsonValue::Null,
        }));
    }
    let Some(crew) = resolved.crews.get(parsed.name) else {
        return Ok(Some(JsonValue::Null));
    };
    Ok(Some(match parsed.field {
        "kind" => serde_json::json!(orbit_types::identity::AGENT_CREW_KIND),
        "model" => serde_json::json!(crew.assignment.model),
        "provider" => serde_json::json!(crew.assignment.provider),
        "effort" => serde_json::json!(crew.assignment.effort),
        "description" => serde_json::json!(crew.description),
        "tags" => serde_json::json!(crew.tags),
        "enabled" => serde_json::json!(crew.enabled),
        "env" => serde_json::json!(crew.env),
        "vision" => serde_json::json!(crew.vision),
        _ => JsonValue::Null,
    }))
}
