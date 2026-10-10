use std::sync::Arc;

use orbit_common::governance::friction::{
    DEFAULT_FRICTION_TAGS, FRICTION_TITLE_MAX_CHARS, friction_tag_aliases_literal,
};
use orbit_common::protocol::tool_schema::tool_input_schema_for;
use orbit_types::tool::{
    McpToolAnnotations, McpToolDefinition, McpToolScope, ToolParam, ToolSchema,
};
use rmcp::model::{JsonObject, Tool, ToolAnnotations};
use serde_json::{Value, json};

use super::name_map::sanitize_tool_name;

pub(super) fn schema_to_tool(
    schema: ToolSchema,
    input_schema: JsonObject,
    annotations: Option<McpToolAnnotations>,
) -> Tool {
    let description = schema.description.clone();
    let advertised_name = sanitize_tool_name(&schema.name);
    let tool = Tool::new(advertised_name, description, Arc::new(input_schema));
    match annotations {
        Some(annotations) => tool.with_annotations(tool_annotations(annotations)),
        None => tool,
    }
}

/// The rmcp hints for one definition; an unset hint stays unadvertised.
fn tool_annotations(annotations: McpToolAnnotations) -> ToolAnnotations {
    let mut hints = ToolAnnotations::new();
    hints.read_only_hint = annotations.read_only;
    hints.destructive_hint = annotations.destructive;
    hints.idempotent_hint = annotations.idempotent;
    hints.open_world_hint = annotations.open_world;
    hints
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

/// An id-routed task tool on a bound authoritative session: the binding is
/// the default, and a remote prefix is refused rather than relayed.
const ID_ROUTED_BOUND_SELECTOR_DESCRIPTION: &str = "Workspace selector for the authoritative server: a registered workspace name, a logical \
     workspace ID (`ws_*`), or an absolute path registered on that server. Optional in this \
     session, which is already bound to a workspace — by `orbit mcp serve --workspace` at \
     launch or `_meta.orbit.workspace` at initialize. Pass it to address a different \
     registered workspace. An id whose prefix belongs to another registered host is refused \
     with `task_prefix_remote`, naming that host; this server does not relay. Never inferred \
     from the server process cwd.";

/// An id-routed task tool on an unbound authoritative session: the id
/// addresses the task, so the selector is optional.
const ID_ROUTED_UNBOUND_SELECTOR_DESCRIPTION: &str = "Optional workspace selector for the authoritative server: a registered workspace \
     name, a logical workspace ID (`ws_*`), or an absolute path registered on that server. \
     Omitted, this session, which is bound to no workspace, resolves the task id through this \
     server's task registry. An id whose prefix belongs to another registered host is refused \
     with `task_prefix_remote`, naming that host; this server does not relay. Never inferred \
     from the server process cwd.";

/// An id-routed task tool on the federated mux: an id-only call goes to the
/// host the id's prefix names.
const FEDERATED_ID_ROUTED_SELECTOR_DESCRIPTION: &str = "Optional. Omit it to deliver the call to the host the task id's prefix names. To address \
     a specific workspace instead, copy the `selector` field from federated \
     `orbit.workspace.list`; do not parse or construct the token. A read there may be another \
     host's mirror, and that host refuses a write to a task it does not hold.";

/// How this session advertises the workspace selector on workspace-scoped tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SelectorAdvertisement {
    /// v1 local/remote MCP: registered name, `ws_*`, or absolute path.
    Authoritative(WorkspaceBinding),
    /// Federated mux: copy `selector` from federated `orbit.workspace.list`.
    Federated,
}

impl WorkspaceBinding {
    fn selector_description(self, id_routed: bool) -> &'static str {
        match (self, id_routed) {
            (Self::Bound, false) => BOUND_SESSION_SELECTOR_DESCRIPTION,
            (Self::Unbound, false) => UNBOUND_SESSION_SELECTOR_DESCRIPTION,
            (Self::Bound, true) => ID_ROUTED_BOUND_SELECTOR_DESCRIPTION,
            (Self::Unbound, true) => ID_ROUTED_UNBOUND_SELECTOR_DESCRIPTION,
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
    let id_routed = crate::federated::is_id_routed_tool(&definition.schema.name);
    match advertisement {
        SelectorAdvertisement::Authoritative(binding) => {
            ensure_authoritative_selector(schema, definition, binding, id_routed);
        }
        SelectorAdvertisement::Federated => ensure_federated_selector(schema, id_routed),
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
    id_routed: bool,
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
                "description": binding.selector_description(id_routed),
            }),
        );
    }
    if binding == WorkspaceBinding::Unbound && !id_routed {
        require_property(schema, WORKSPACE_SELECTOR_PARAM);
    }
}

fn ensure_federated_selector(schema: &mut JsonObject, id_routed: bool) {
    let Some(properties) = selector_properties(schema) else {
        return;
    };
    // Replace any v1 local wording a tool declared itself. Federated callers
    // copy the list token; an id-routed task tool may instead omit it and
    // route by the id's prefix.
    let description = if id_routed {
        FEDERATED_ID_ROUTED_SELECTOR_DESCRIPTION
    } else {
        FEDERATED_SELECTOR_DESCRIPTION
    };
    properties.insert(
        WORKSPACE_SELECTOR_PARAM.to_string(),
        json!({
            "type": "string",
            "description": description,
        }),
    );
    if id_routed {
        // A tool that declared its own selector as required is still
        // addressable by id here.
        unrequire_property(schema, WORKSPACE_SELECTOR_PARAM);
    } else {
        require_property(schema, WORKSPACE_SELECTOR_PARAM);
    }
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

fn unrequire_property(schema: &mut JsonObject, property: &str) {
    if let Some(required) = schema.get_mut("required").and_then(Value::as_array_mut) {
        required.retain(|name| name != property);
    }
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
        // A query or tag narrows any search; `kind: friction` alone lists
        // friction records.
        schema.insert(
            "allOf".to_string(),
            json!([{ "anyOf": [
                { "required": ["query"] },
                { "required": ["tag"] },
                { "required": ["kind"], "properties": { "kind": { "const": "friction" } } }
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
