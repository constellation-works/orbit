//! Adapter-owned desktop entrypoints and static MCP Apps resource.
//! All data and mutation authority remains behind McpHost.

use std::sync::Arc;

use super::schema::WORKSPACE_SELECTOR_DESCRIPTION;
use crate::presentation_definition::{
    PRESENTATION_ACTIONS, PRESENTATION_TOOL_NAME, PRESENTATION_TOOL_TITLE, PresentationKind,
    is_presentation_tool, presentation_action,
};
use orbit_common::OrbitError;
use orbit_types::tool::{McpPresentationRead, McpToolDefinition};
use rmcp::model::{MetaObject, Resource, ResourceContents, Tool, ToolAnnotations};
use serde_json::{Value, json};

pub(super) const RESOURCE_URI: &str = "ui://orbit/control-center/v1/index.html";
pub(super) const LEGACY_RESOURCE_URI: &str = "ui://orbit/task-panel/v1/index.html";
const MIME: &str = "text/html;profile=mcp-app";
pub(super) const TASK_FIELDS: [&str; 6] = [
    "id",
    "title",
    "status",
    "updated_at",
    "description",
    "acceptance_criteria",
];

pub(super) fn is_presentation(name: &str) -> bool {
    is_presentation_tool(name)
}

pub(super) fn available(definitions: &[McpToolDefinition]) -> bool {
    reader(definitions, McpPresentationRead::Task).is_some()
}

/// Select a host-declared presentation reader with its required read-only hint.
pub(super) fn reader(
    definitions: &[McpToolDefinition],
    role: McpPresentationRead,
) -> Option<&McpToolDefinition> {
    definitions.iter().find(|definition| {
        definition.rules.presentation_read == Some(role)
            && (role != McpPresentationRead::Task
                || definition
                    .annotations
                    .is_some_and(|hints| hints.read_only == Some(true)))
    })
}

pub(super) fn tools(_federated: bool) -> Vec<Tool> {
    let actions = PRESENTATION_ACTIONS
        .iter()
        .map(|action| action.action)
        .collect::<Vec<_>>();
    let entrypoints = PRESENTATION_ACTIONS
        .iter()
        .map(|action| json!({ "type": action.entrypoint }))
        .collect::<Vec<_>>();
    let schema = json!({
        "type": "object", "additionalProperties": false,
        "properties": {
            "action": {
                "type": "string", "enum": actions,
                "description": "open: the control center, or a task or run panel given workspace and id. inspect: a task or run panel (workspace, id required)."
            },
            "workspace": { "type": "string", "minLength": 1, "description": WORKSPACE_SELECTOR_DESCRIPTION },
            "id": { "type": "string", "minLength": 1, "description": "Public task key or run ID." },
            "kind": {"type":"string", "enum":["task","run"], "default":"task"}
        },
        "required": ["action"],
        "dependentRequired": { "id": ["workspace"], "kind": ["workspace", "id"] }
    });
    vec![
        Tool::new(
            PRESENTATION_TOOL_NAME,
            "Open the Orbit control center or a selected task or run panel.",
            Arc::new(schema.as_object().cloned().unwrap_or_default()),
        )
        .with_title(PRESENTATION_TOOL_TITLE)
        .with_annotations(
            ToolAnnotations::new()
                .read_only(true)
                .destructive(false)
                .idempotent(true)
                .open_world(false),
        )
        .with_meta(MetaObject(
            json!({
                "ui": { "resourceUri": RESOURCE_URI },
                "openai/ui": { "entrypoints": entrypoints }
            })
            .as_object()
            .cloned()
            .unwrap_or_default(),
        )),
    ]
}

pub(super) fn resource() -> Resource {
    Resource::new(RESOURCE_URI, "orbit-control-center-v1")
        .with_title("Orbit control center")
        .with_mime_type(MIME)
}

pub(super) fn resource_content(uri: &str) -> ResourceContents {
    // Entirely static: task content is never interpolated into executable HTML.
    let html = include_str!("../../assets/task-panel/index.html")
        .replace(
            "/* ORBIT_PANEL_STYLE */",
            include_str!("../../assets/task-panel/css/task-panel.css"),
        )
        .replace(
            "/* ORBIT_PANEL_SCRIPT */",
            &[
                include_str!("../../../orbit-web/assets/dashboard/vendor/marked.umd.js"),
                include_str!("../../../orbit-web/assets/dashboard/vendor/purify.min.js"),
                include_str!("../../assets/task-panel/js/presentation.js"),
                include_str!("../../assets/task-panel/js/drain.js"),
                include_str!("../../assets/task-panel/js/automation.js"),
                include_str!("../../assets/task-panel/js/task-panel.js"),
            ]
            .join("\n"),
        );
    ResourceContents::text(html, uri)
        .with_mime_type(MIME)
        .with_meta(MetaObject(
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

/// A destination and optional entity, validated independently of client state.
pub(super) struct Selection {
    pub workspace: String,
    pub id: Option<String>,
    pub kind: String,
}

/// Core still validates selector ownership, task identity and authority.
pub(super) fn selection(input: &Value) -> Result<Option<Selection>, OrbitError> {
    let mut arguments = input
        .as_object()
        .ok_or_else(|| OrbitError::InvalidInput("presentation arguments must be an object".into()))?
        .clone();
    let names = || {
        PRESENTATION_ACTIONS
            .iter()
            .map(|action| action.action)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let kind = match arguments.remove("action") {
        Some(Value::String(action)) => {
            presentation_action(action.trim())
                .ok_or_else(|| {
                    OrbitError::InvalidInput(format!(
                        "unknown presentation action `{action}`; expected one of {}",
                        names()
                    ))
                })?
                .kind
        }
        _ => {
            return Err(OrbitError::InvalidInput(format!(
                "presentation requires `action`: one of {}",
                names()
            )));
        }
    };
    if arguments
        .keys()
        .any(|key| !matches!(key.as_str(), "workspace" | "id" | "kind"))
    {
        return Err(OrbitError::InvalidInput(
            "presentation accepts only action, workspace, id and kind".into(),
        ));
    }
    let opener = kind == PresentationKind::Open;
    if opener && arguments.is_empty() {
        return Ok(None);
    }
    let field = |name| {
        arguments
            .get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty() && value.len() <= 2048)
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "presentation requires an explicit non-empty {name}"
                ))
            })
    };
    let workspace = field("workspace")?;
    let id = if opener && !arguments.contains_key("id") && !arguments.contains_key("kind") {
        None
    } else {
        Some(field("id")?)
    };
    let kind = if arguments.contains_key("kind") {
        field("kind")?
    } else {
        "task".into()
    };
    if !matches!(kind.as_str(), "task" | "run") {
        return Err(OrbitError::InvalidInput(
            "presentation kind must be task or run".into(),
        ));
    }
    Ok(Some(Selection {
        workspace,
        id,
        kind,
    }))
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
