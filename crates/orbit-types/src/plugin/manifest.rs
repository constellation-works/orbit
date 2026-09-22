//! `plugin.yaml` v2: one manifest declaring a namespace and its tools.
//!
//! Every struct is `deny_unknown_fields` (the `RoutineDefinition` posture).
//! Every section is consumed: `tools` become the registry, CLI and MCP
//! surfaces, `definitions`/`skills`/`config` are installed on enable, `web`
//! feeds the dashboard's `plugins` group and `tests` drives `orbit plugin
//! test`.

use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::namespace::{is_valid_namespace, is_valid_verb};
use super::template::validate_template;
use super::version::{SemverRange, Version};

pub const MANIFEST_FILE_NAME: &str = "plugin.yaml";
pub const MANIFEST_SCHEMA_VERSION: u32 = 2;
pub const MANIFEST_KIND: &str = "Plugin";

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
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    /// `orbit` claims `orbit.<ns>.*`; honoured only for a verified first-party source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<PluginOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    #[serde(default, skip_serializing_if = "PluginRequires::is_empty")]
    pub requires: PluginRequires,
    pub backend: PluginBackend,
    /// Requested, never granted here (§4.1).
    #[serde(default, skip_serializing_if = "PluginPermissions::is_empty")]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orbit: Option<String>,
    /// Protocol major; a mismatch refuses enable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_api: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub platforms: Vec<String>,
    /// Host programs the backend spawns.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub programs: Vec<String>,
}

impl PluginRequires {
    fn is_empty(&self) -> bool {
        self.orbit.is_none()
            && self.host_api.is_none()
            && self.platforms.is_empty()
            && self.programs.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginBackend {
    #[serde(rename = "type")]
    pub backend_type: PluginBackendType,
    /// Relative to the plugin root, or absolute.
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_pass: Vec<String>,
    /// Orbit tools the backend may call back into.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub orbit_tools: Vec<String>,
}

impl PluginPermissions {
    fn is_empty(&self) -> bool {
        self.fs.is_empty()
            && self.network == PluginNetworkPermission::None
            && self.env_pass.is_empty()
            && self.orbit_tools.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginFsPermissions {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write: Vec<String>,
}

impl PluginFsPermissions {
    fn is_empty(&self) -> bool {
        self.read.is_empty() && self.write.is_empty()
    }
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
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub execution_kind: PluginExecutionKind,
    #[serde(default)]
    pub mcp_scope: PluginMcpScope,
    /// JSON Schema object, or `{ $ref: <path inside the plugin root> }`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    /// Optional override of the derived `orbit <ns> <verb>` clap shape (§4.6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    /// The subcommand name under `orbit <ns>`; defaults to the tool verb.
    #[serde(default)]
    pub verb: Option<String>,
    /// Top-level `input_schema` properties promoted to positional arguments,
    /// in order. Each still takes its type from the schema.
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

/// One dashboard panel (§4.7): the output of a `read_only` tool, drawn by
/// the generic renderer named in `render`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginWebPanel {
    pub id: String,
    #[serde(default)]
    pub title: String,
    /// `tool:<verb>`, naming one of this plugin's `read_only` tools.
    pub source: String,
    #[serde(default)]
    pub render: PluginPanelRender,
    #[serde(default)]
    pub group: PluginPanelGroup,
}

impl PluginWebPanel {
    /// The verb after `tool:`, when the source has that form.
    pub fn source_verb(&self) -> Option<&str> {
        self.source
            .strip_prefix(PANEL_SOURCE_TOOL_PREFIX)
            .map(str::trim)
            .filter(|verb| !verb.is_empty())
    }
}

/// The only source form v1 accepts.
pub const PANEL_SOURCE_TOOL_PREFIX: &str = "tool:";

/// The schemes a `spec.web.links[].url` may use (§4.7).
pub const LINK_URL_SCHEMES: &[&str] = &["http://", "https://"];

/// How the dashboard draws a panel's JSON (§4.7).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginPanelRender {
    /// An object as label/value pairs.
    Kv,
    /// An array of objects as one table; columns are the union of keys.
    Table,
    /// A string (or an object's `markdown`/`text` field) as sanitised Markdown.
    Markdown,
    /// Pretty-printed JSON.
    #[default]
    Json,
}

impl PluginPanelRender {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Kv => "kv",
            Self::Table => "table",
            Self::Markdown => "markdown",
            Self::Json => "json",
        }
    }
}

/// Which section of a plugin's dashboard card a panel lands in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginPanelGroup {
    #[default]
    Diagnostics,
    Operations,
    Config,
}

impl PluginPanelGroup {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Diagnostics => "diagnostics",
            Self::Operations => "operations",
            Self::Config => "config",
        }
    }
}

/// A plain tile pointing at a plugin-hosted UI (§4.7). `url` may use the
/// manifest template variables, typically `{{config.<key>}}` for a port.
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
            let field = format!("spec.permissions.env_pass[{index}]");
            if name.trim().is_empty() || name.contains('=') {
                return Err(PluginManifestError::new(
                    field,
                    format!("'{name}' is not an environment variable name"),
                ));
            }
            // `ORBIT_*` is Orbit's own execution envelope, not something a
            // manifest can request more of: the host stamps the plugin's
            // envelope unconditionally, and privilege-bearing names in this
            // namespace (`ORBIT_OPERATOR`, `ORBIT_WORKSPACE_CLAIM_TOKEN`) must
            // never reach a plugin child even by explicit request.
            if name.starts_with("ORBIT_") {
                return Err(PluginManifestError::new(
                    field,
                    format!(
                        "'{name}' is an Orbit-reserved variable and cannot be requested via \
                         env_pass; the plugin envelope provides Orbit context automatically"
                    ),
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
            if let Some(input_schema) = &tool.input_schema {
                validate_plugin_cli_flags(
                    input_schema,
                    &format!("spec.tools[{index}].input_schema"),
                )?;
            }
        }
        self.validate_definition_paths()?;
        self.validate_web()?;
        Ok(())
    }

    /// `spec.web` (§4.7): a panel reads exactly one declared `read_only`
    /// tool; a link is a title and a template-valid URL.
    ///
    /// A panel over a mutating tool is refused here, at the manifest, so the
    /// dashboard never has to decide at request time whether a source may
    /// be served to an unauthenticated session.
    fn validate_web(&self) -> Result<(), PluginManifestError> {
        let Some(web) = &self.spec.web else {
            return Ok(());
        };
        let mut seen = std::collections::BTreeSet::new();
        for (index, panel) in web.panels.iter().enumerate() {
            let field = format!("spec.web.panels[{index}]");
            if !is_valid_verb(&panel.id) {
                return Err(PluginManifestError::new(
                    format!("{field}.id"),
                    format!(
                        "'{}' is not a valid panel id: use lowercase letters, digits, '_' or '-'",
                        panel.id
                    ),
                ));
            }
            if !seen.insert(panel.id.as_str()) {
                return Err(PluginManifestError::new(
                    format!("{field}.id"),
                    format!("panel '{}' is declared more than once", panel.id),
                ));
            }
            let Some(verb) = panel.source_verb() else {
                return Err(PluginManifestError::new(
                    format!("{field}.source"),
                    format!(
                        "panel '{}' has source '{}'; a panel source is `tool:<verb>` naming one \
                         of this plugin's tools",
                        panel.id, panel.source
                    ),
                ));
            };
            let Some(tool) = self.spec.tools.iter().find(|tool| tool.name == verb) else {
                return Err(PluginManifestError::new(
                    format!("{field}.source"),
                    format!(
                        "panel '{}' sources tool '{verb}', which this manifest does not declare",
                        panel.id
                    ),
                ));
            };
            if tool.execution_kind != PluginExecutionKind::ReadOnly {
                return Err(PluginManifestError::new(
                    format!("{field}.source"),
                    format!(
                        "panel '{}' sources tool '{verb}', which is `execution_kind: mutating`; \
                         a dashboard panel may only read a `read_only` tool",
                        panel.id
                    ),
                ));
            }
        }
        for (index, link) in web.links.iter().enumerate() {
            let field = format!("spec.web.links[{index}]");
            if link.title.trim().is_empty() {
                return Err(PluginManifestError::new(
                    format!("{field}.title"),
                    "must not be empty",
                ));
            }
            if link.url.trim().is_empty() {
                return Err(PluginManifestError::new(
                    format!("{field}.url"),
                    "must not be empty",
                ));
            }
            // The dashboard sets a link's URL straight onto an anchor, so
            // the scheme is fixed here: a `javascript:` or `data:` tile would
            // be plugin-authored script running in the operator's session.
            if !LINK_URL_SCHEMES
                .iter()
                .any(|scheme| link.url.to_ascii_lowercase().starts_with(scheme))
            {
                return Err(PluginManifestError::new(
                    format!("{field}.url"),
                    format!(
                        "'{}' must be an http:// or https:// URL; a link tile is a plain \
                         hyperlink to a plugin-hosted UI",
                        link.url
                    ),
                ));
            }
            validate_template(&link.url, &format!("{field}.url"))?;
        }
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
        for (index, pattern) in self.spec.tests.iter().enumerate() {
            validate_plugin_relative_path(pattern, &format!("spec.tests[{index}]"))?;
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
