use std::sync::Arc;

use orbit_common::governance::friction::{
    DEFAULT_FRICTION_TAGS, FRICTION_TITLE_MAX_CHARS, friction_tag_aliases_literal,
};
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

/// A tool's own declared input schema, as advertised before the host adds its
/// selector: every keyword verbatim except the root `type`, which is always
/// `"object"` — MCP's `inputSchema` must describe an object and `tools/call`
/// arguments always are one (design §4.2).
pub(super) fn declared_input_schema(declared: &JsonObject) -> JsonObject {
    let mut schema = declared.clone();
    schema.insert("type".to_string(), json!("object"));
    schema
}

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

/// The selector is a routing argument only when the plugin did not declare it
/// as part of its own input. Built-in tools retain their existing inputs.
pub(super) fn host_owns_plugin_selector(definition: &McpToolDefinition) -> bool {
    !definition.schema.builtin
        && definition.scope == McpToolScope::WorkspaceRequired
        && !definition
            .schema
            .parameters
            .iter()
            .any(|parameter| parameter.name == WORKSPACE_SELECTOR_PARAM)
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
    let Some(properties) = selector_properties(schema) else {
        return;
    };
    if !properties.contains_key(WORKSPACE_SELECTOR_PARAM) {
        properties.insert(
            WORKSPACE_SELECTOR_PARAM.to_string(),
            json!({
                "type": "string",
                "description": binding.selector_description(),
            }),
        );
    }
    if binding == WorkspaceBinding::Unbound {
        require_property(schema, WORKSPACE_SELECTOR_PARAM);
    }
}

fn ensure_federated_selector(schema: &mut JsonObject) {
    let Some(properties) = selector_properties(schema) else {
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
    require_property(schema, WORKSPACE_SELECTOR_PARAM);
}

/// The `properties` map the selector joins. A declared schema may name no
/// properties at all; it still needs the selector to be callable.
fn selector_properties(schema: &mut JsonObject) -> Option<&mut JsonObject> {
    schema
        .entry("properties")
        .or_insert_with(|| json!({}))
        .as_object_mut()
}

fn require_property(schema: &mut JsonObject, property: &str) {
    let required = schema.entry("required").or_insert_with(|| json!([]));
    if let Some(required) = required.as_array_mut()
        && !required.iter().any(|name| name == property)
    {
        required.push(json!(property));
    }
}

#[cfg(test)]
pub(crate) fn build_input_schema(tool_name: &str, params: &[ToolParam]) -> JsonObject {
    build_input_schema_with_friction_taxonomy(tool_name, params, None)
}

pub(crate) fn build_input_schema_with_friction_taxonomy(
    tool_name: &str,
    params: &[ToolParam],
    taxonomy: Option<&[(String, String)]>,
) -> JsonObject {
    let mut schema = tool_input_schema_for(tool_name, params);
    decorate_friction_schema(&mut schema, tool_name, taxonomy);
    if tool_name == "orbit.search" {
        if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
            if let Some(query) = properties.get_mut("query").and_then(Value::as_object_mut) {
                query.insert("minLength".to_string(), json!(1));
            }
            if let Some(alternatives) = properties
                .get_mut("tag")
                .and_then(|tag| tag.get_mut("anyOf"))
                .and_then(Value::as_array_mut)
            {
                for alternative in alternatives {
                    if let Some(option) = alternative.as_object_mut() {
                        match option.get("type").and_then(Value::as_str) {
                            Some("string") => {
                                option.insert("minLength".to_string(), json!(1));
                            }
                            Some("array") => {
                                option.insert("minItems".to_string(), json!(1));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        schema.insert(
            "allOf".to_string(),
            json!([{ "anyOf": [
                { "required": ["query"] },
                { "required": ["tag"] }
            ] }]),
        );
    }
    schema
}

fn decorate_friction_schema(
    schema: &mut JsonObject,
    tool_name: &str,
    taxonomy: Option<&[(String, String)]>,
) {
    if !matches!(tool_name, "orbit.friction.add" | "orbit.friction.update") {
        return;
    }
    let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) else {
        return;
    };

    if let Some(title) = properties.get_mut("title").and_then(Value::as_object_mut) {
        title.insert("maxLength".to_string(), json!(FRICTION_TITLE_MAX_CHARS));
    }

    let Some(tags) = properties.get_mut("tags").and_then(Value::as_object_mut) else {
        return;
    };
    let using_workspace_taxonomy = taxonomy.is_some_and(|entries| !entries.is_empty());
    let fallback;
    let entries = match taxonomy.filter(|entries| !entries.is_empty()) {
        Some(entries) => entries,
        None => {
            fallback = DEFAULT_FRICTION_TAGS
                .iter()
                .map(|(tag, description)| ((*tag).to_string(), (*description).to_string()))
                .collect::<Vec<_>>();
            &fallback
        }
    };
    let values = entries
        .iter()
        .map(|(tag, _description)| tag.clone())
        .collect::<Vec<_>>();
    if let Some(alternatives) = tags.get_mut("anyOf").and_then(Value::as_array_mut) {
        for alternative in alternatives {
            if alternative.get("type").and_then(Value::as_str) == Some("array")
                && let Some(items) = alternative.get_mut("items").and_then(Value::as_object_mut)
            {
                items.insert("enum".to_string(), json!(values));
            }
        }
    }
    let vocabulary = entries
        .iter()
        .map(|(tag, description)| format!("`{tag}` — {description}"))
        .collect::<Vec<_>>()
        .join("; ");
    let fallback_note = if using_workspace_taxonomy {
        ""
    } else {
        " The bound workspace's `.orbit/frictions/tags.yaml` may extend this default vocabulary."
    };
    tags.insert(
        "description".to_string(),
        Value::String(format!(
            "Friction taxonomy tags as a string or array. Vocabulary: {vocabulary}. Accepted aliases: {}.{fallback_note}",
            friction_tag_aliases_literal()
        )),
    );
}

#[cfg(test)]
pub(super) fn property_for(param_type: &str) -> JsonObject {
    tool_parameter_schema(param_type)
}
