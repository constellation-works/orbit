//! MCP machine-local discovery definitions and execution.

use std::collections::BTreeSet;

use orbit_common::protocol::tool_input::optional_csv_or_string_list_alias;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::tool::{
    McpToolAnnotations, McpToolDefinition, McpToolDefinitionError, McpToolScope, ToolParam,
    ToolSchema, validate_mcp_tool_definitions,
};
use orbit_types::workspace::{Workspace, WorkspaceRegistry, WorkspaceStatus};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Private wire name used by the federated mux to inspect every local checkout.
///
/// This is deliberately absent from `discovery_tool_definitions`: direct v1
/// clients continue to see and call only `orbit.workspace.list`, whose Active
/// filter is part of that surface. The destination server recognizes this
/// exact private request without adding it to the advertised tool surface.
pub const FEDERATED_DESTINATION_WORKSPACE_LIST_TOOL: &str =
    "orbit_federated_destination_workspace_list";

/// What a destination says about itself on every discovery envelope.
///
/// `machine_id` is the v1 key. The other four are additive [ORB-14448]: a
/// destination built before them omits them, and a reader treats each as
/// unknown rather than guessing. They let `orbit host` read a host's identity
/// and skew from the host itself instead of from operator-maintained config.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostFacts {
    pub machine_id: String,
    /// The destination's `[machine] name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_name: Option<String>,
    /// The destination's `[machine] task_prefix`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_prefix: Option<String>,
    /// The destination's running binary version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_version: Option<String>,
    /// The destination's distributed-drain pull-protocol fingerprint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_fingerprint: Option<String>,
}

impl HostFacts {
    /// Read the facts a discovery envelope carries. Only `machine_id` is
    /// required; a non-string optional field reads as unknown.
    pub fn from_envelope(content: &Value) -> Option<Self> {
        let text = |key: &str| content[key].as_str().map(ToOwned::to_owned);
        Some(Self {
            machine_id: text("machine_id")?,
            machine_name: text("machine_name"),
            task_prefix: text("task_prefix"),
            binary_version: text("binary_version"),
            protocol_fingerprint: text("protocol_fingerprint"),
        })
    }

    fn envelope(&self, workspaces: Vec<&Workspace>) -> Value {
        let mut envelope = serde_json::to_value(self).unwrap_or_else(|_| json!({}));
        envelope["machine_id"] = json!(self.machine_id);
        envelope["workspaces"] = json!(workspaces);
        envelope
    }
}

/// `include` value that attaches each workspace's effective crews to its row.
pub const WORKSPACE_LIST_INCLUDE_CREWS: &str = "crews";

pub(super) fn discovery_tool_definitions() -> Result<Vec<McpToolDefinition>, McpToolDefinitionError>
{
    let definitions = vec![workspace_list_definition()];
    validate_mcp_tool_definitions(&definitions)?;
    Ok(definitions)
}

/// The `include` parameter shared by the machine-local and federated lists.
pub fn workspace_list_include_param() -> ToolParam {
    ToolParam {
        name: "include".to_string(),
        description: "Optional extra detail per workspace row. `crews` adds the workspace's \
                      effective configured crews and default crew as `crews`, read on the \
                      machine that holds the checkout, or `crews_error` when its \
                      configuration cannot be read."
            .to_string(),
        param_type: "string_list".to_string(),
        required: false,
    }
}

/// Whether a workspace-list call asked for crews. An unknown `include` value
/// is refused rather than ignored, so a typo does not read as "no crews".
pub fn workspace_list_includes_crews(input: &Value) -> Result<bool, OrbitError> {
    let include = optional_csv_or_string_list_alias(input, &["include"])?.unwrap_or_default();
    let mut crews = false;
    for value in include {
        if value == WORKSPACE_LIST_INCLUDE_CREWS {
            crews = true;
        } else {
            return Err(OrbitError::InvalidInput(format!(
                "unknown `include` value '{value}'; expected `{WORKSPACE_LIST_INCLUDE_CREWS}`"
            )));
        }
    }
    Ok(crews)
}

/// Registry-wide discovery accepts no workspace selector.
fn workspace_list_definition() -> McpToolDefinition {
    McpToolDefinition::new(
        ToolSchema {
            name: "orbit.workspace.list".to_string(),
            description: "List active workspaces with a checkout registered on this machine, \
                          optionally with each one's configured crews."
                .to_string(),
            parameters: vec![workspace_list_include_param()],
            builtin: true,
        },
        McpToolScope::Global,
    )
    .with_annotations(Some(McpToolAnnotations::READ_ONLY))
}

pub fn execute_discovery_tool(
    name: &str,
    registry: &WorkspaceRegistry,
    host: &HostFacts,
) -> Result<Value, OrbitError> {
    match name {
        "orbit.workspace.list" => {
            let workspaces = locally_bound_workspaces(registry)
                .into_iter()
                .filter(|workspace| workspace.status == WorkspaceStatus::Active)
                .collect::<Vec<_>>();
            Ok(host.envelope(workspaces))
        }
        _ => Err(OrbitError::not_found(NotFoundKind::Tool, name.to_string())),
    }
}

/// Project every workspace with a checkout registered on this destination.
///
/// Unlike the public v1 discovery tool, this internal federated path retains
/// Invalid rows so the mux can report their checkout health instead of
/// silently dropping them.
pub fn execute_federated_workspace_discovery(
    registry: &WorkspaceRegistry,
    host: &HostFacts,
) -> Value {
    host.envelope(locally_bound_workspaces(registry))
}

fn locally_bound_workspaces(registry: &WorkspaceRegistry) -> Vec<&Workspace> {
    let local_workspace_ids = registry
        .checkouts
        .iter()
        .map(|checkout| checkout.workspace_id.as_str())
        .collect::<BTreeSet<_>>();
    registry
        .workspaces
        .iter()
        .filter(|workspace| local_workspace_ids.contains(workspace.id.as_str()))
        .collect()
}
