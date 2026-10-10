//! The `plugin.yaml` v2 data model.

use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::super::build::PluginBuildSpec;
use super::PluginWebSection;

pub const MANIFEST_FILE_NAME: &str = "plugin.yaml";
/// The directory a plugin source keeps its plugin in. It is the plugin root:
/// it holds [`MANIFEST_FILE_NAME`], and it is the only tree installed.
pub const PLUGIN_DIR_NAME: &str = ".orbit-plugin";
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
    /// Named credentials the operator sets with `orbit plugin secret set`.
    /// Only names are declared here; values live in the host's secret store.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<PluginSecretSpec>,
    /// How a `git+` source pinned to a commit builds the files its backend
    /// needs (`docs/design/plugins/3_install_time_build.md`). Runs only with
    /// the operator's per-install consent; every other source must already
    /// carry the declared outputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<PluginBuildSpec>,
}

/// One `spec.secrets` entry. The name is namespaced to the plugin: two
/// plugins declaring `api_token` hold two unrelated secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginSecretSpec {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Whether the backend may replace the value itself (an OAuth refresh
    /// token that the provider rotates on every refresh).
    #[serde(default)]
    pub rotatable: bool,
}

/// Longest `spec.secrets[].name` a manifest may declare.
pub const MAX_SECRET_NAME_LEN: usize = 64;

/// Whether `name` is an acceptable `spec.secrets[].name`: a lowercase letter,
/// then lowercase letters, digits, `_` or `-`, at most
/// [`MAX_SECRET_NAME_LEN`] bytes. The name becomes a key in the host's secret
/// store and a CLI argument, so it is kept to a spelling that needs no
/// quoting anywhere.
pub fn is_valid_secret_name(name: &str) -> bool {
    name.len() <= MAX_SECRET_NAME_LEN
        && name
            .bytes()
            .next()
            .is_some_and(|first| first.is_ascii_lowercase())
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        })
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
    /// A stdio MCP server Orbit spawns once per caller context per runtime
    /// (workspace and allowed-tools intersection) and proxies
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
