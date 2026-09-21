//! `plugin.yaml` v2: one manifest declaring a namespace and its tools.
//!
//! Every struct is `deny_unknown_fields` (the `RoutineDefinition` posture).
//! Sections that later phases consume — `definitions`, `skills`, `config`,
//! `web`, `tests` — are parsed here so a manifest written against the full
//! design still validates, and are otherwise unused.

use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::namespace::{is_valid_namespace, is_valid_verb};
use super::template::validate_template;
use super::version::{SemverRange, Version};

pub const MANIFEST_FILE_NAME: &str = "plugin.yaml";
pub const MANIFEST_SCHEMA_VERSION: u32 = 2;
pub const MANIFEST_KIND: &str = "Plugin";

/// A manifest rejection. `field` is the dotted path of the offending key so
/// every diagnostic names what to fix.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct PluginManifestError {
    pub field: String,
    pub message: String,
}

impl PluginManifestError {
    pub fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl Display for PluginManifestError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub kind: String,
    pub metadata: PluginMetadata,
    pub spec: PluginSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginMetadata {
    /// The namespace: `graph` owns `graph.*`, `orbit graph`, `[plugins.graph]`.
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub publisher: Option<String>,
    /// `orbit` claims `orbit.<ns>.*`; honoured only for a verified first-party source.
    #[serde(default)]
    pub origin: Option<PluginOrigin>,
    #[serde(default)]
    pub homepage: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginOrigin {
    Orbit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginSpec {
    #[serde(default)]
    pub requires: PluginRequires,
    pub backend: PluginBackend,
    /// Requested, never granted here (§4.1).
    #[serde(default)]
    pub permissions: PluginPermissions,
    #[serde(default)]
    pub tools: Vec<PluginToolSpec>,
    /// Definition files this plugin ships (§4.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definitions: Option<PluginDefinitions>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<PluginConfigSection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web: Option<PluginWebSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginRequires {
    /// Semver range on the host binary.
    #[serde(default)]
    pub orbit: Option<String>,
    /// Protocol major; a mismatch refuses enable.
    #[serde(default)]
    pub host_api: Option<u32>,
    #[serde(default)]
    pub platforms: Vec<String>,
    /// Host programs the backend spawns.
    #[serde(default)]
    pub programs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginBackend {
    #[serde(rename = "type")]
    pub backend_type: PluginBackendType,
    /// Relative to the plugin root, or absolute.
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub sandbox: PluginSandbox,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginBackendType {
    /// One process per call with the JSON envelope on stdin/stdout (§4.2).
    Exec,
    /// A stdio MCP server Orbit spawns once per runtime and proxies
    /// `<ns>.<verb>` to as `tools/call` (§4.2).
    Mcp,
}

impl PluginBackendType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exec => "exec",
            Self::Mcp => "mcp",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginSandbox {
    #[default]
    Default,
    None,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginPermissions {
    #[serde(default)]
    pub fs: PluginFsPermissions,
    #[serde(default)]
    pub network: PluginNetworkPermission,
    #[serde(default)]
    pub env_pass: Vec<String>,
    /// Orbit tools the backend may call back into.
    #[serde(default)]
    pub orbit_tools: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginFsPermissions {
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub write: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginNetworkPermission {
    #[default]
    None,
    Loopback,
    Any,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginToolSpec {
    /// The verb: canonical `<ns>.<verb>`, MCP `<ns>_<verb>`.
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub execution_kind: PluginExecutionKind,
    #[serde(default)]
    pub mcp_scope: PluginMcpScope,
    /// JSON Schema object, or `{ $ref: <path inside the plugin root> }`.
    #[serde(default)]
    pub input_schema: Option<Value>,
    #[serde(default)]
    pub output_schema: Option<Value>,
    /// Optional override of the derived clap shape (a later phase).
    #[serde(default)]
    pub cli: Option<PluginCliShape>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginExecutionKind {
    ReadOnly,
    Mutating,
}

impl PluginExecutionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Mutating => "mutating",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginMcpScope {
    #[default]
    Workspace,
    Global,
    /// Registered active for `orbit tool run`, never advertised over MCP.
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginCliShape {
    #[serde(default)]
    pub verb: Option<String>,
    #[serde(default)]
    pub positional: Vec<String>,
}

/// `spec.definitions`: glob lists of the definition files a plugin ships.
///
/// Activities and jobs become the `plugin:<ns>` catalog layer; routines and
/// auto-tasks are seeded as `enabled: false` templates on enable (§3).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginDefinitions {
    #[serde(default)]
    pub activities: Vec<String>,
    #[serde(default)]
    pub jobs: Vec<String>,
    #[serde(default)]
    pub routines: Vec<String>,
    #[serde(default)]
    pub auto_tasks: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginConfigSection {
    #[serde(default)]
    pub schema: Option<String>,
    #[serde(default)]
    pub defaults: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginWebSection {
    #[serde(default)]
    pub panels: Vec<PluginWebPanel>,
    #[serde(default)]
    pub links: Vec<PluginWebLink>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginWebPanel {
    pub id: String,
    #[serde(default)]
    pub title: String,
    pub source: String,
    #[serde(default)]
    pub render: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginWebLink {
    pub title: String,
    pub url: String,
}

impl PluginManifest {
    /// Structural validation that needs no filesystem: versions, kinds,
    /// namespace and verb spelling, duplicate tools, backend type.
    pub fn validate_structure(&self) -> Result<(), PluginManifestError> {
        if self.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(PluginManifestError::new(
                "schemaVersion",
                format!(
                    "unsupported plugin manifest schemaVersion {}; expected {MANIFEST_SCHEMA_VERSION}",
                    self.schema_version
                ),
            ));
        }
        if self.kind != MANIFEST_KIND {
            return Err(PluginManifestError::new(
                "kind",
                format!("expected '{MANIFEST_KIND}', found '{}'", self.kind),
            ));
        }
        if !is_valid_namespace(&self.metadata.name) {
            return Err(PluginManifestError::new(
                "metadata.name",
                format!(
                    "'{}' is not a valid namespace: use lowercase letters, digits, '_' or '-', \
                     starting with a letter, and not the reserved 'orbit'",
                    self.metadata.name
                ),
            ));
        }
        self.metadata
            .version
            .parse::<Version>()
            .map_err(|error| PluginManifestError::new("metadata.version", error.to_string()))?;
        if let Some(range) = &self.spec.requires.orbit {
            SemverRange::parse(range).map_err(|error| {
                PluginManifestError::new("spec.requires.orbit", error.to_string())
            })?;
        }
        if self.spec.backend.command.trim().is_empty() {
            return Err(PluginManifestError::new(
                "spec.backend.command",
                "must not be empty",
            ));
        }
        if self.spec.backend.timeout_ms == Some(0) {
            return Err(PluginManifestError::new(
                "spec.backend.timeout_ms",
                "must be greater than zero",
            ));
        }
        for (index, path) in self.spec.permissions.fs.read.iter().enumerate() {
            validate_template(path, &format!("spec.permissions.fs.read[{index}]"))?;
        }
        for (index, path) in self.spec.permissions.fs.write.iter().enumerate() {
            validate_template(path, &format!("spec.permissions.fs.write[{index}]"))?;
        }
        for (index, name) in self.spec.permissions.env_pass.iter().enumerate() {
            if name.trim().is_empty() || name.contains('=') {
                return Err(PluginManifestError::new(
                    format!("spec.permissions.env_pass[{index}]"),
                    format!("'{name}' is not an environment variable name"),
                ));
            }
        }
        if self.spec.tools.is_empty() {
            return Err(PluginManifestError::new(
                "spec.tools",
                "a plugin must declare at least one tool",
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for (index, tool) in self.spec.tools.iter().enumerate() {
            let field = format!("spec.tools[{index}].name");
            if !is_valid_verb(&tool.name) {
                return Err(PluginManifestError::new(
                    field,
                    format!(
                        "'{}' is not a valid tool verb: use lowercase letters, digits, '_' or '-'",
                        tool.name
                    ),
                ));
            }
            if !seen.insert(tool.name.as_str()) {
                return Err(PluginManifestError::new(
                    field,
                    format!("tool '{}' is declared more than once", tool.name),
                ));
            }
            for (schema, key) in [
                (tool.input_schema.as_ref(), "input_schema"),
                (tool.output_schema.as_ref(), "output_schema"),
            ] {
                if let Some(schema) = schema
                    && !schema.is_object()
                {
                    return Err(PluginManifestError::new(
                        format!("spec.tools[{index}].{key}"),
                        "must be a JSON Schema object or `{ $ref: <path> }`",
                    ));
                }
            }
        }
        self.validate_definition_paths()?;
        Ok(())
    }

    /// Every path the manifest names outside `spec.tools`: definition globs,
    /// skill directories and the config schema. All are plugin-root relative.
    fn validate_definition_paths(&self) -> Result<(), PluginManifestError> {
        if let Some(definitions) = &self.spec.definitions {
            for (patterns, key) in [
                (&definitions.activities, "activities"),
                (&definitions.jobs, "jobs"),
                (&definitions.routines, "routines"),
                (&definitions.auto_tasks, "auto_tasks"),
            ] {
                for (index, pattern) in patterns.iter().enumerate() {
                    validate_plugin_relative_path(
                        pattern,
                        &format!("spec.definitions.{key}[{index}]"),
                    )?;
                }
            }
        }
        for (index, skill) in self.spec.skills.iter().enumerate() {
            validate_plugin_relative_path(skill, &format!("spec.skills[{index}]"))?;
        }
        if let Some(config) = &self.spec.config {
            if let Some(schema) = &config.schema {
                validate_plugin_relative_path(schema, "spec.config.schema")?;
            }
            if let Some(defaults) = &config.defaults
                && !defaults.is_object()
            {
                return Err(PluginManifestError::new(
                    "spec.config.defaults",
                    "must be a table of `[plugins.<ns>]` keys",
                ));
            }
        }
        Ok(())
    }

    /// Whether the manifest claims the reserved `orbit.<ns>.*` namespace.
    pub fn claims_first_party_namespace(&self) -> bool {
        self.metadata.origin == Some(PluginOrigin::Orbit)
    }

    /// `plugin:<ns>@<version>`: the provenance a seeded definition, a managed
    /// skill and the catalog layer all carry (§4.4).
    pub fn provenance(&self) -> String {
        plugin_provenance_label(&self.metadata.name, &self.metadata.version)
    }
}

/// The provenance label for a plugin at one version.
pub fn plugin_provenance_label(namespace: &str, version: &str) -> String {
    format!("plugin:{namespace}@{version}")
}

/// Reject a manifest path that would leave the plugin root before it is ever
/// joined to it: an absolute path, a `..` component, or an empty one.
///
/// Containment is still re-checked after canonicalisation at load; this is the
/// pure-data half so a manifest can be refused without touching a filesystem.
pub fn validate_plugin_relative_path(value: &str, field: &str) -> Result<(), PluginManifestError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(PluginManifestError::new(field, "must not be empty"));
    }
    if trimmed.starts_with('/') || trimmed.contains('\\') {
        return Err(PluginManifestError::new(
            field,
            format!("'{value}' must be a relative path inside the plugin root"),
        ));
    }
    if trimmed
        .split('/')
        .any(|component| component == ".." || component.is_empty())
    {
        return Err(PluginManifestError::new(
            field,
            format!("'{value}' must not contain an empty or '..' path component"),
        ));
    }
    Ok(())
}
