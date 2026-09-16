use std::sync::Arc;

use orbit_common::protocol::tool_schema::tool_input_schema_for;
#[cfg(test)]
use orbit_common::protocol::tool_schema::tool_parameter_schema;
use orbit_types::tool::{McpToolDefinition, McpToolScope, ToolParam, ToolSchema};
use rmcp::model::{JsonObject, Tool};
use serde_json::{Value, json};

use super::name_map::sanitize_tool_name;

pub(super) fn schema_to_tool(schema: ToolSchema, input_schema: JsonObject) -> Tool {
    let description = schema.description.clone();
    let advertised_name = sanitize_tool_name(&schema.name);
    Tool::new(advertised_name, description, Arc::new(input_schema))
}

/// Canonical name of the authoritative server's workspace selector.
pub(crate) const WORKSPACE_SELECTOR_PARAM: &str = "workspace";

/// Whether the session being served already carries a workspace selector.
///
/// The two states are advertised differently because they place different
/// obligations on the caller: a bound session may omit the selector, an
/// unbound one is refused without it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum WorkspaceBinding {
    Bound,
    Unbound,
}

const BOUND_SESSION_SELECTOR_DESCRIPTION: &str = "Workspace selector for the authoritative server: a registered workspace name, a logical \
     workspace ID (`ws_*`), or an absolute path registered on that server. Optional in this \
     session, which is already bound to a workspace — by `orbit mcp serve --workspace` at \
     launch or `_meta.orbit.workspace` at initialize. Pass it to address a different \
     registered workspace; never inferred from the server process cwd.";

const UNBOUND_SESSION_SELECTOR_DESCRIPTION: &str = "Workspace selector for the authoritative server: a registered workspace name, a logical \
     workspace ID (`ws_*`), or an absolute path registered on that server. Required in this \
     session, which is bound to no workspace, so a call that omits it is refused. Sessions \
     bound by `orbit mcp serve --workspace` at launch or `_meta.orbit.workspace` at \
     initialize may omit it; first call `orbit_workspace_list` and reuse a returned `ws_*` ID. \
     If none is listed, run `orbit init` and then `orbit workspace init` from the project \
     directory. Never inferred from the server process cwd.";

/// Federated callers copy the list token; they must not mint a v1 local form.
const FEDERATED_SELECTOR_DESCRIPTION: &str = "Copy the `selector` field from federated `orbit.workspace.list` to address a workspace. \
     Do not parse or construct the token. A call without a host-qualified selector is refused.";

/// How this session advertises the workspace selector on workspace-scoped tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SelectorAdvertisement {
    /// v1 local/remote MCP: registered name, `ws_*`, or absolute path.
    Authoritative(WorkspaceBinding),
    /// Federated mux: copy `selector` from federated `orbit.workspace.list`.
    Federated,
}

impl WorkspaceBinding {
    fn selector_description(self) -> &'static str {
        match self {
            Self::Bound => BOUND_SESSION_SELECTOR_DESCRIPTION,
            Self::Unbound => UNBOUND_SESSION_SELECTOR_DESCRIPTION,
        }
    }
}

/// Advertise the workspace selector on every workspace-scoped tool.
pub(super) fn ensure_workspace_selector(
    schema: &mut JsonObject,
    definition: &McpToolDefinition,
    advertisement: SelectorAdvertisement,
) {
    if definition.scope != McpToolScope::WorkspaceRequired {
        return;
    }
    match advertisement {
        SelectorAdvertisement::Authoritative(binding) => {
            ensure_authoritative_selector(schema, definition, binding);
        }
        SelectorAdvertisement::Federated => ensure_federated_selector(schema),
    }
}

fn ensure_authoritative_selector(
    schema: &mut JsonObject,
    definition: &McpToolDefinition,
    binding: WorkspaceBinding,
) {
    // `orbit.task.show` still opens a workspace runtime, but `id` is globally
    // resolved by default [ORB-10961]. The generic selector text would make
    // clients inject cwd, initialize metadata, or a linked-worktree runtime
    // identity. The tool declares its own optional filter instead.
    if definition.schema.name == "orbit.task.show" {
        return;
    }
    let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) else {
        return;
    };
    if properties.contains_key(WORKSPACE_SELECTOR_PARAM) {
        return;
    }
    properties.insert(
        WORKSPACE_SELECTOR_PARAM.to_string(),
        json!({
            "type": "string",
            "description": binding.selector_description(),
        }),
    );
}

fn ensure_federated_selector(schema: &mut JsonObject) {
    let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) else {
        return;
    };
    // Replace any v1 local wording a tool declared itself — including
    // `orbit.task.show`'s optional id-only filter. Federated callers must copy
    // the list token; id-only default does not survive two machines.
    properties.insert(
        WORKSPACE_SELECTOR_PARAM.to_string(),
        json!({
            "type": "string",
            "description": FEDERATED_SELECTOR_DESCRIPTION,
        }),
    );
}

pub(crate) fn build_input_schema(tool_name: &str, params: &[ToolParam]) -> JsonObject {
    tool_input_schema_for(tool_name, params)
}

#[cfg(test)]
pub(super) fn property_for(param_type: &str) -> JsonObject {
    tool_parameter_schema(param_type)
}
