use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolSessionContext {
    /// Runtime-owned attempt binding. Ordinary JSON session metadata cannot
    /// create authority; transport adapters propagate it explicitly.
    #[serde(skip)]
    pub worker_invocation: Option<super::WorkerInvocation>,
    /// Legacy caller-supplied workspace address. This value is deliberately
    /// untrusted until an adapter/runtime resolves it to [`Self::workspace_id`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Stable logical workspace identity after trusted local resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// Stable machine label claimed by the caller for audit correlation. It
    /// is self-declared metadata, not an authenticated principal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_machine_id: Option<String>,
    /// [ORB-12725] `caller_host_id` is read for one release so a session
    /// envelope from an older peer still deserializes.
    #[serde(
        default,
        alias = "caller_host_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub caller_machine_name: Option<String>,
    /// Stable identity of the accepting machine, derived by the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_machine_id: Option<String>,
    /// [ORB-12725] `process_host_id` is read for one release; see
    /// [`Self::caller_machine_name`].
    #[serde(
        default,
        alias = "process_host_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub process_machine_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<McpTransport>,
    /// Per-invocation correlation ID created by the accepting process. This is
    /// independent of the legacy MCP session/call identifiers below so local
    /// and remote entry points share one trace field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// Best-effort caller network address observed by the accepting process.
    /// It is audit metadata only and is never an authenticated identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_ip: Option<String>,
    /// Complete effective session grants. This is a set, never a scalar
    /// ceiling; callers authorize by membership.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub effective_capabilities: BTreeSet<McpCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_call_id: Option<String>,
    /// Identity the client claimed for itself at session initialize, already
    /// reduced by `orbit_types::telemetry::normalize_self_reported_actor`
    /// [ORB-10890].
    ///
    /// Session-scoped rather than per-call: `initialize` is the one point in
    /// the MCP protocol where the client describes itself, and a per-call
    /// claim would let the same session present a different identity on every
    /// tool call. It reaches the audit row and nothing else — it is not an
    /// authenticated principal and never contributes to `role`, agent/model
    /// resolution, or any authorization decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_reported_actor: Option<String>,
    /// Orchestrator crew this session attributes newly created tasks to,
    /// configured by `orbit mcp serve --orchestrator` [ORB-11313].
    ///
    /// Session-scoped like [`Self::workspace`] and, like it, purely a
    /// default: an explicit per-call `orchestrator` wins, and the value is
    /// resolved against the target workspace's crews on every call rather
    /// than trusted as written. It is attribution only — it grants no
    /// capability, never contributes to [`Self::effective_capabilities`] or
    /// any authorization decision, and never selects the execution crew or
    /// the model a task runs under.
    ///
    /// It is a configured default, not authenticated evidence of the model
    /// answering a given call: a persistent MCP connection outlives a model
    /// switch on the client side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator: Option<String>,
}

impl ToolSessionContext {
    pub fn with_workspace(workspace: impl Into<String>) -> Self {
        Self {
            workspace: Some(workspace.into()),
            ..Self::default()
        }
    }

    /// Construct the trusted defaults for the local standalone MCP adapter.
    pub fn trusted_local(
        workspace_id: Option<String>,
        machine_id: Option<String>,
        machine_name: Option<String>,
    ) -> Self {
        Self {
            workspace: None,
            worker_invocation: None,
            workspace_id,
            caller_machine_id: machine_id.clone(),
            caller_machine_name: machine_name.clone(),
            process_machine_id: machine_id,
            process_machine_name: machine_name,
            transport: Some(McpTransport::Local),
            trace_id: None,
            caller_ip: None,
            effective_capabilities: BTreeSet::from([McpCapability::Agent]),
            origin_session_id: None,
            mcp_call_id: None,
            // Trusted defaults describe the accepting machine; a claim only
            // ever arrives from the client, at initialize.
            self_reported_actor: None,
            // The standalone adapter is launched per call, not configured as
            // a session, so it carries no attribution default.
            orchestrator: None,
        }
    }

    pub fn has_capability(&self, capability: McpCapability) -> bool {
        self.effective_capabilities.contains(&capability)
    }

    /// The caller label an SSH-originated session forwarded, for attribution.
    ///
    /// Absent on a local session, whose [`Self::caller_machine_id`] is the
    /// accepting machine's own identity and therefore says nothing about a
    /// caller elsewhere. Even when present it is a label the caller chose: it
    /// names a machine in the audit trail and never contributes to an
    /// authorization decision.
    pub fn remote_caller_machine_id(&self) -> Option<&str> {
        if self.transport != Some(McpTransport::SshMcp) {
            return None;
        }
        self.caller_machine_id.as_deref()
    }
}

/// Transport that delivered an MCP call to the executing process.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum McpTransport {
    Local,
    SshMcp,
}

impl Display for McpTransport {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Local => "local",
            Self::SshMcp => "ssh-mcp",
        })
    }
}

impl FromStr for McpTransport {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "local" => Ok(Self::Local),
            "ssh-mcp" => Ok(Self::SshMcp),
            other => Err(format!("unknown MCP transport: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolParam {
    pub name: String,
    pub description: String,
    pub param_type: String,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: Vec<ToolParam>,
    pub builtin: bool,
}

/// Whether an MCP tool requires a logical workspace in its trusted session.
///
/// Existing tools are workspace-scoped by default. Registry-wide discovery is
/// the narrow exception: a global tool operates without selecting or inferring
/// a workspace.
#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "kebab-case")]
pub enum McpToolScope {
    #[default]
    WorkspaceRequired,
    Global,
}

/// A capability granted to an invocation context.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "lowercase")]
pub enum McpCapability {
    Agent,
    Operator,
    /// In-process grant stamped by a managed run so it can perform the
    /// destructive operation it exists to perform.
    Runner,
}

impl Display for McpCapability {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Agent => "agent",
            Self::Operator => "operator",
            Self::Runner => "runner",
        })
    }
}

impl FromStr for McpCapability {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "agent" => Ok(Self::Agent),
            "operator" => Ok(Self::Operator),
            "runner" => Ok(Self::Runner),
            other => Err(format!("unknown MCP capability: {other}")),
        }
    }
}

/// A schema paired with the only routing fact MCP needs for exposure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpToolDefinition {
    pub schema: ToolSchema,
    pub scope: McpToolScope,
    /// The tool's own JSON Schema for its input, when it declares one — a
    /// plugin tool's manifest `input_schema`. MCP advertises it as written
    /// instead of the schema derived from the flat `schema.parameters`, which
    /// cannot express `enum`, bounds, `default` or nested shapes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
}

impl McpToolDefinition {
    pub fn new(schema: ToolSchema, scope: McpToolScope) -> Self {
        Self {
            schema,
            scope,
            input_schema: None,
        }
    }

    pub fn with_input_schema(mut self, input_schema: Option<Value>) -> Self {
        self.input_schema = input_schema;
        self
    }
}

/// Why a canonical MCP definition set is invalid.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum McpToolDefinitionError {
    #[error("canonical MCP tool name must not be empty")]
    EmptyCanonicalName,
    #[error("duplicate canonical MCP tool name: {0}")]
    DuplicateCanonicalName(String),
    #[error("duplicate advertised MCP tool name: {0}")]
    DuplicateAdvertisedName(String),
}

/// Convert a canonical Orbit tool name to its MCP-advertised form.
pub fn mcp_advertised_tool_name(canonical_name: &str) -> String {
    canonical_name.replace('.', "_")
}

/// Whether a task requirement is an exact canonical tool name rather than a
/// wildcard, prefix, or transport spelling.
pub fn is_exact_canonical_tool_name(name: &str) -> bool {
    if name.is_empty() || name.trim() != name || name.contains('*') || name.contains(',') {
        return false;
    }
    let mut segments = name.split('.');
    let valid_segment = |segment: &str| {
        !segment.is_empty()
            && segment.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
            })
    };
    segments.next().is_some_and(valid_segment)
        && segments.next().is_some_and(valid_segment)
        && segments.all(valid_segment)
}

/// Validate schema-adjacent MCP definitions, including both canonical and advertised names.
pub fn validate_mcp_tool_definitions(
    definitions: &[McpToolDefinition],
) -> Result<(), McpToolDefinitionError> {
    let mut canonical_names = BTreeSet::new();
    let mut advertised_names = BTreeSet::new();
    for definition in definitions {
        let canonical_name = definition.schema.name.as_str();
        if canonical_name.trim().is_empty() {
            return Err(McpToolDefinitionError::EmptyCanonicalName);
        }
        if !canonical_names.insert(canonical_name) {
            return Err(McpToolDefinitionError::DuplicateCanonicalName(
                canonical_name.to_string(),
            ));
        }
        let advertised_name = mcp_advertised_tool_name(canonical_name);
        if !advertised_names.insert(advertised_name.clone()) {
            return Err(McpToolDefinitionError::DuplicateAdvertisedName(
                advertised_name,
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredTool {
    pub name: String,
    pub path: String,
    pub description: String,
    pub enabled: bool,
    pub builtin: bool,
    #[serde(default)]
    pub parameters: Vec<ToolParam>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExecutionResult {
    pub success: bool,
    /// Whether the process supervisor terminated the child after its deadline.
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub output: Option<Value>,
}
