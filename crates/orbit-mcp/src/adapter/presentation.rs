//! Adapter-owned, read-only MCP Apps prototype. No application authority lives here.

use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_types::tool::McpToolDefinition;
use rmcp::model::{Meta, Resource, ResourceContents, Tool, ToolAnnotations};
use serde_json::{Value, json};

pub(super) const RESOURCE_URI: &str = "ui://orbit/task-panel/v1/index.html";
const MIME: &str = "text/html;profile=mcp-app";
pub(super) const OPEN: &str = "orbit_ui_open";
pub(super) const INSPECT: &str = "orbit_ui_inspect";
pub(super) const TASK_FIELDS: [&str; 6] = [
    "id",
    "title",
    "status",
    "updated_at",
    "description",
    "acceptance_criteria",
];

pub(super) fn is_presentation(name: &str) -> bool {
    matches!(name, OPEN | INSPECT | "orbit.ui.open" | "orbit.ui.inspect")
}

pub(super) fn available(definitions: &[McpToolDefinition]) -> bool {
    definitions.iter().any(|definition| {
        definition.schema.name == "orbit.task.show"
            && definition
                .annotations
                .is_some_and(|hints| hints.read_only == Some(true))
    })
}

pub(super) fn tools(federated: bool) -> Vec<Tool> {
    [ (OPEN, "Open Orbit task panel", "global"),
      (INSPECT, "Inspect Orbit task", "thread") ]
        .into_iter()
        .map(|(name, title, entrypoint)| {
            let selector = if federated {
                "Copy the opaque selector from orbit_workspace_list. A bare workspace ID is refused."
            } else {
                "Explicit registered workspace name, logical workspace ID, or registered absolute checkout path. Never inferred from the session or cwd."
            };
            let mut schema = json!({
                "type": "object", "additionalProperties": false,
                "properties": {
                    "workspace": { "type": "string", "minLength": 1, "description": selector },
                    "id": { "type": "string", "minLength": 1, "description": "Public task key returned by Orbit." }
                }
            });
            if name == INSPECT {
                schema["required"] = json!(["workspace", "id"]);
            } else {
                schema["dependentRequired"] = json!({ "id": ["workspace"], "workspace": ["id"] });
            }
            Tool::new(name, "Open a read-only task panel. Selected tasks are read through orbit_task_show with an explicit workspace filter; opening grants no authority.",
                Arc::new(schema.as_object().cloned().unwrap_or_default()))
                .with_title(title)
                .with_annotations(ToolAnnotations::new().read_only(true).destructive(false).idempotent(true).open_world(false))
                .with_meta(Meta(json!({
                    "ui": { "resourceUri": RESOURCE_URI },
                    "openai/ui": { "entrypoints": [{ "type": entrypoint }] }
                }).as_object().cloned().unwrap_or_default()))
        })
        .collect()
}

pub(super) fn resource() -> Resource {
    Resource::new(RESOURCE_URI, "orbit-task-panel-v1")
        .with_title("Orbit task panel")
        .with_mime_type(MIME)
}

pub(super) fn resource_content() -> ResourceContents {
    // Entirely static: task content is never interpolated into executable HTML.
    let html = include_str!("../../assets/task-panel/index.html")
        .replace(
            "/* ORBIT_PANEL_STYLE */",
            include_str!("../../assets/task-panel/css/task-panel.css"),
        )
        .replace(
            "/* ORBIT_PANEL_SCRIPT */",
            include_str!("../../assets/task-panel/js/task-panel.js"),
        );
    ResourceContents::text(html, RESOURCE_URI)
        .with_mime_type(MIME)
        .with_meta(Meta(
            json!({
                "ui": { "prefersBorder": true,
                    "csp": { "connectDomains": [], "resourceDomains": [], "frameDomains": [] } },
                "openai/ui": { "availableDisplayModes": ["inline", "fullscreen"] }
            })
            .as_object()
            .cloned()
            .unwrap_or_default(),
        ))
}

/// Validate only presentation inputs. Core still owns selector and task validation.
pub(super) fn selection(name: &str, input: &Value) -> Result<Option<(String, String)>, OrbitError> {
    let arguments = input.as_object().ok_or_else(|| {
        OrbitError::InvalidInput("presentation arguments must be an object".into())
    })?;
    if arguments
        .keys()
        .any(|key| !matches!(key.as_str(), "workspace" | "id"))
    {
        return Err(OrbitError::InvalidInput(
            "presentation accepts only workspace and id".into(),
        ));
    }
    if matches!(name, OPEN | "orbit.ui.open") && arguments.is_empty() {
        return Ok(None);
    }
    let field = |name| {
        arguments
            .get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "presentation requires an explicit non-empty {name}"
                ))
            })
    };
    Ok(Some((field("workspace")?, field("id")?)))
}

pub(super) fn empty_panel() -> Value {
    json!({ "schema_version": 1, "workspace": null, "task": null })
}

pub(super) fn panel(workspace: String, id: &str, task: Value) -> Result<Value, OrbitError> {
    if task.get("id").and_then(Value::as_str) != Some(id)
        || !task.get("title").is_some_and(Value::is_string)
        || !task.get("updated_at").is_some_and(Value::is_string)
    {
        return Err(OrbitError::InvalidInput(
            "incompatible task read response; panel unavailable".into(),
        ));
    }
    Ok(json!({ "schema_version": 1, "workspace": workspace, "task": task }))
}
