//! Effective config values and their source metadata.

use std::path::{Path, PathBuf};

/// Which layer supplied a resolved value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigValueSourceKind {
    /// A compiled-in default.
    BuiltIn,
    /// An environment variable.
    Environment,
    /// The global `config.toml`.
    Global,
    /// The workspace `config.toml`.
    Workspace,
}

impl ConfigValueSourceKind {
    /// Stable label used in `orbit config show` output.
    pub fn label(self) -> &'static str {
        match self {
            Self::BuiltIn => "built-in",
            Self::Environment => "environment",
            Self::Global => "global",
            Self::Workspace => "workspace",
        }
    }
}

/// Where one resolved value came from, including the file when there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigValueSource {
    pub(super) kind: ConfigValueSourceKind,
    pub(super) path: Option<PathBuf>,
}

impl ConfigValueSource {
    /// The layer that supplied the value.
    pub fn kind(&self) -> ConfigValueSourceKind {
        self.kind
    }

    /// The file that supplied the value, for file-backed layers.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

/// Why a layer that defines a key did not supply the effective value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowReason {
    /// A higher layer set the same key.
    Overridden,
    /// A security key (`WORKSPACE_REPLACE_ONLY_KEYS`) that the workspace
    /// file must restate to keep: it never inherits from global once a
    /// distinct workspace file exists.
    NotInherited,
}

impl ShadowReason {
    /// Stable token used in `orbit config show --json`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Overridden => "overridden",
            Self::NotInherited => "not-inherited",
        }
    }
}

/// A layer that defines a key without supplying the effective value.
///
/// This is what makes the two surprising cases legible in `config show`: a
/// global value a workspace overrode, and a global security value that was
/// deliberately not inherited.
#[derive(Debug, Clone, PartialEq)]
pub struct ShadowedConfigValue {
    /// Layer that defines the shadowed value.
    pub layer: ConfigValueSourceKind,
    /// The value that layer defines, projected as JSON.
    pub value: serde_json::Value,
    /// Why it is not the effective value.
    pub reason: ShadowReason,
}

/// The layer and value a key resolves to once the layer that supplies it
/// stops setting it, when that prediction is available. For a configured
/// crew, built-in values are the entry's defaults, not the shipped crew's fields.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigFallback {
    /// Layer that would supply the value.
    pub layer: ConfigValueSourceKind,
    /// The value that layer would supply, projected as JSON. Null when it is
    /// the built-in layer and the key has no value there.
    pub value: serde_json::Value,
}

/// Three-way state of one resolved value.
///
/// `unset` and `default` are different facts that both used to render as
/// `[built-in]`: one setting has no value at all, the other has a compiled-in
/// one that is actually in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigValueState {
    /// A config file (or the environment) supplied the value.
    Set,
    /// No file supplies it; the compiled-in default is in force.
    Default,
    /// No file supplies it and there is no default: the key has no value.
    Unset,
}

impl ConfigValueState {
    /// Stable token used in `orbit config show --json`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::Default => "default",
            Self::Unset => "unset",
        }
    }
}

/// One resolved key with its value and provenance.
#[derive(Debug, Clone)]
pub struct EffectiveConfigValue {
    /// Dotted config key.
    pub key: String,
    /// Resolved value, projected as JSON.
    pub value: serde_json::Value,
    /// Layer the value came from.
    pub source: ConfigValueSource,
    /// Layers that define the key without supplying the effective value,
    /// highest-precedence first. Empty for the ordinary case.
    pub shadowed_by: Vec<ShadowedConfigValue>,
    /// The global value, admitted for registry keys in the effective crew/settings context,
    /// including when shadowed. `None` when the global file does not set the
    /// key or its value cannot be admitted in that context. `shadowed_by`
    /// retains the raw file value for provenance compatibility.
    pub global_value: Option<serde_json::Value>,
    /// The value Orbit ships with, before any file, the environment, or a
    /// configured crew registry; JSON null when the key has none. A crew
    /// field reports the built-in crew of that name. A plugin key reports
    /// null: its defaults belong to the plugin.
    pub built_in: serde_json::Value,
    /// What the key resolves to if the layer in `source` stopped setting it.
    /// A replace-only security key the workspace sets falls to the built-in
    /// value, never the global one, because the workspace file still exists.
    /// `None` for a built-in value, which no layer can remove, or when the
    /// removal cannot be admitted in the current context. In a surviving
    /// configured crew table, optional fields use entry defaults; a required
    /// model/provider without a valid lower file value has no fallback.
    pub fallback: Option<ConfigFallback>,
}

impl EffectiveConfigValue {
    /// Whether the value is set by a layer, defaulted, or absent entirely.
    pub fn state(&self) -> ConfigValueState {
        match self.source.kind() {
            ConfigValueSourceKind::BuiltIn if self.value.is_null() => ConfigValueState::Unset,
            ConfigValueSourceKind::BuiltIn => ConfigValueState::Default,
            _ => ConfigValueState::Set,
        }
    }
}

/// Every resolved key with provenance, for `orbit config show`/`get`.
#[derive(Debug, Clone)]
pub struct EffectiveConfig {
    pub(super) snapshot: crate::registry::ConfigSnapshot,
    pub(super) values: Vec<EffectiveConfigValue>,
}

impl EffectiveConfig {
    /// Resolved value for one admitted key, including projected crew fields.
    ///
    /// Configured crew effort is present only when the assignment set it.
    /// A live `crews.<name>.effort` key whose crew exists but omitted the
    /// field returns JSON null rather than inventing a provider default.
    pub fn value_for(&self, key: &str) -> Option<serde_json::Value> {
        if let Some(value) = self.snapshot.value_for(key) {
            return Some(value);
        }
        if let Some(entry) = self.values.iter().find(|entry| entry.key == key) {
            return Some(entry.value.clone());
        }
        if let Ok(Some(parsed)) = crate::plugins::parse_plugin_field_key(key) {
            let prefix = format!("plugins.{}.", parsed.namespace);
            if self
                .values
                .iter()
                .any(|entry| entry.key.starts_with(&prefix))
            {
                return Some(serde_json::Value::Null);
            }
        }
        if let Ok(Some(parsed)) = crate::registry::parse_crew_field_key(key) {
            let prefix = format!("crews.{}.", parsed.name);
            if self
                .values
                .iter()
                .any(|entry| entry.key.starts_with(&prefix))
            {
                return Some(serde_json::Value::Null);
            }
        }
        None
    }

    /// Every resolved value, sorted by key.
    pub fn values(&self) -> &[EffectiveConfigValue] {
        &self.values
    }
}
