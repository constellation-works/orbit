//! Installed plugins and their dashboard panels (design §4.7).
//!
//! Two endpoints, both read-only:
//!
//! * `GET /api/plugins` — every installed or pinned plugin with its enable
//!   state, diagnostics, tools, panels and link tiles.
//! * `GET /api/plugins/<ns>/panels/<id>` — the JSON one declared panel's
//!   source tool returns.
//!
//! A panel source is always a `read_only` tool: the manifest cannot declare
//! a panel over a mutating one (`orbit plugin validate` refuses it, naming
//! the panel), and the read re-checks the loaded manifest before executing.
//! That is what makes a panel safe to serve to any dashboard session while
//! the dashboard's mutations still require an operator session.

use axum::extract::Path;
use axum::response::{IntoResponse, Json, Response};
use orbit_core::adapter::command::PluginSummary;
use orbit_types::plugin::PluginStatus;
use serde_json::{Value, json};

use super::{blocking, not_found, validate_id};
use crate::state::Ws;

pub(super) async fn list_plugins(Ws(runtime): Ws) -> Response {
    match blocking("list plugins", move || runtime.list_plugins()).await {
        Ok(plugins) => {
            Json(Value::Array(plugins.iter().map(plugin_to_json).collect())).into_response()
        }
        Err(response) => *response,
    }
}

pub(super) async fn read_panel(
    Ws(runtime): Ws,
    Path((namespace, panel)): Path<(String, String)>,
) -> Response {
    // Both are manifest-shaped identifiers; rejecting anything else keeps a
    // path segment from reaching the lookup as an arbitrary string.
    for (label, value) in [("plugin", &namespace), ("panel", &panel)] {
        if let Err(message) = validate_id(value) {
            return super::bad_request(format!("{label} {message}"));
        }
    }
    // An absent plugin or panel is a 404, not a server fault: the dashboard
    // asks for what a previous `/api/plugins` listed, and a plugin disabled
    // since then is exactly this answer. `map_runtime_error` only knows the
    // task/job kinds, so the projection happens here.
    match blocking("read plugin panel", move || {
        Ok(runtime.read_plugin_panel(&namespace, &panel))
    })
    .await
    {
        Ok(Ok(output)) => Json(json!({ "output": output })).into_response(),
        Ok(Err(orbit_core::OrbitError::NotFound { id, .. })) => not_found(id),
        Ok(Err(error)) => super::map_runtime_error(error),
        Err(response) => *response,
    }
}

fn plugin_to_json(summary: &PluginSummary) -> Value {
    json!({
        "name": summary.name,
        "version": summary.version,
        "status": summary.status.as_str(),
        "enabled": summary.status == PluginStatus::Active,
        "source": summary.source,
        "install_path": summary.install_path,
        "manifest_digest": summary.manifest_digest,
        "publisher": summary.publisher,
        "description": summary.description,
        "first_party": summary.first_party,
        "pinned": summary.pinned,
        "unsandboxed": summary.unsandboxed,
        "granted": summary.granted,
        "certified_orbit_version": summary.certified_orbit_version,
        "diagnostic": summary.diagnostic,
        "permissions": summary
            .permissions
            .iter()
            .map(|permission| json!({
                "grant": permission.grant.as_str(),
                "requested": permission.requested,
                "granted": permission.granted,
            }))
            .collect::<Vec<_>>(),
        "tools": summary
            .tools
            .iter()
            .map(|tool| json!({
                "name": tool.name,
                "advertised_name": tool.advertised_name,
                "execution_kind": tool.execution_kind.as_str(),
                "mcp_scope": tool.mcp_scope,
                "active": tool.active,
            }))
            .collect::<Vec<_>>(),
        "panels": summary
            .panels
            .iter()
            .map(|panel| json!({
                "id": panel.id,
                "title": panel.title,
                "tool": panel.tool,
                "render": panel.render.as_str(),
                "group": panel.group.as_str(),
            }))
            .collect::<Vec<_>>(),
        "links": summary
            .links
            .iter()
            .map(|link| json!({ "title": link.title, "url": link.url }))
            .collect::<Vec<_>>(),
    })
}
