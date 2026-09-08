use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolSessionContext {
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_host_id: Option<String>,
    /// Stable identity of the process host, derived by the accepting server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_machine_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_host_id: Option<String>,
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
    /// The destination-side statement that capped this session, present only
    /// on a remote-originated session [ORB-11052].
    ///
    /// Its presence is what distinguishes "this destination granted the
    /// caller these capabilities" from "the local server process stamped its
    /// own authority", so the audit trail can tell a downgraded caller from
    /// one that never asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_caller_grant: Option<RemoteCallerGrant>,
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
        host_id: Option<String>,
    ) -> Self {
        Self {
            workspace: None,
            workspace_id,
            caller_machine_id: machine_id.clone(),
            caller_host_id: host_id.clone(),
            process_machine_id: machine_id,
            process_host_id: host_id,
            transport: Some(McpTransport::Local),
            trace_id: None,
            caller_ip: None,
            effective_capabilities: BTreeSet::from([McpCapability::Agent]),
            origin_session_id: None,
            mcp_call_id: None,
            // Trusted defaults describe the accepting machine; a claim only
            // ever arrives from the client, at initialize.
            self_reported_actor: None,
            remote_caller_grant: None,
            // The standalone adapter is launched per call, not configured as
            // a session, so it carries no attribution default.
            orchestrator: None,
        }
    }

    pub fn has_capability(&self, capability: McpCapability) -> bool {
        self.effective_capabilities.contains(&capability)
    }
}

/// What a destination's callers file granted the caller of a remote-originated
/// MCP session [ORB-11052].
///
/// The session's effective capabilities are this set intersected with what the
/// session's argv requested, so recording the grant separately is what makes a
/// downgrade legible: `effective` alone cannot distinguish a caller that was
/// capped from one that never asked for more.
///
/// `caller_machine_id` is only as strong as [`Self::identity`] says it is:
/// under [`CallerIdentityProof::SelfAsserted`] it is a label the caller chose,
/// and it selects a row rather than proving anything.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteCallerGrant {
    /// Caller identity the destination resolved the grant for.
    pub caller_machine_id: String,
    /// Capabilities the destination is willing to serve that caller.
    pub granted_capabilities: BTreeSet<McpCapability>,
    /// Display path of the file that made the statement, for the denial
    /// message a refused caller has to act on.
    pub source: String,
    /// How [`Self::caller_machine_id`] was established [ORB-11053].
    #[serde(default)]
    pub identity: CallerIdentityProof,
    /// Whether the destination explicitly permits this caller to submit the
    /// trusted-host agent-invocation operation in the resolved workspace.
    ///
    /// This stays separate from `operator`: ordinary operator capability does
    /// not imply permission to start an unsandboxed provider process remotely.
    #[serde(default)]
    pub agent_invoke: bool,
    /// Destination-selected trust mode for [`Self::agent_invoke`]. `None`
    /// preserves the original strict key-bound behavior for older envelopes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_invoke_mode: Option<RemoteAgentInvokeMode>,
}

/// Trust model a destination selected for remote trusted-host invocation.
///
/// This is operation-specific rather than a property of the whole caller row:
/// ordinary remote operator operations keep their existing authorization, and
/// enabling the cooperative mode does not make a self-asserted identity
/// key-bound.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RemoteAgentInvokeMode {
    /// Require the destination-issued forced-command identity introduced with
    /// the original remote agent-invocation grant.
    #[default]
    KeyBound,
    /// Trust the existing SSH OS-account/operator channel while recording the
    /// caller machine ID as self-asserted. This is an accident-prevention
    /// boundary between cooperating users of the same account, not isolation.
    Cooperative,
}

impl Display for RemoteAgentInvokeMode {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::KeyBound => "key-bound",
            Self::Cooperative => "cooperative",
        })
    }
}

/// How a destination established the caller identity it resolved a grant for.
///
/// A destination may run either tier of caller authorization, so the trail has
/// to say which one answered rather than leaving a reader to assume: the two
/// grants look identical once resolved, and only this field separates a row a
/// caller merely named from one it proved it holds the key for.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum CallerIdentityProof {
    /// The caller named itself. It selects a row and proves nothing, so the
    /// grant is an accident guard rather than a boundary.
    #[default]
    SelfAsserted,
    /// sshd authenticated the key whose `authorized_keys` entry names this
    /// caller, and the destination — not the caller — composed the argv that
    /// carries the identity.
    KeyBound,
}

impl Display for CallerIdentityProof {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::SelfAsserted => "self-asserted",
            Self::KeyBound => "key-bound",
        })
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
}

impl McpToolDefinition {
    pub fn new(schema: ToolSchema, scope: McpToolScope) -> Self {
        Self { schema, scope }
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
