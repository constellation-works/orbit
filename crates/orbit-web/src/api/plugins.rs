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

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Json, Response};
use orbit_core::adapter::command::PluginSummary;
use orbit_types::plugin::PluginStatus;
use serde_json::{Value, json};

use super::{blocking, not_found, validate_id};
use crate::state::{DashboardState, Ws};

/// Maximum serialized tool output retained or returned for one panel.
pub(crate) const PANEL_OUTPUT_LIMIT_BYTES: usize = 256 * 1024;

pub(super) async fn list_plugins(Ws(runtime): Ws) -> Response {
    match blocking("list plugins", move || runtime.list_plugins()).await {
        Ok(plugins) => {
            Json(Value::Array(plugins.iter().map(plugin_to_json).collect())).into_response()
        }
        Err(response) => *response,
    }
}

pub(super) async fn read_panel(
    State(state): State<DashboardState>,
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
    let refresh_ms = match runtime.plugin_panel_refresh_ms(&namespace, &panel) {
        Ok(refresh_ms) => refresh_ms,
        Err(orbit_core::OrbitError::NotFound { id, .. }) => return not_found(id),
        Err(error) => return super::map_runtime_error(error),
    };
    let compute_runtime = Arc::clone(&runtime);
    let compute_namespace = namespace.clone();
    let compute_panel = panel.clone();
    match state
        .plugin_panel_memo()
        .get_or_compute(
            &runtime,
            (namespace, panel),
            Duration::from_millis(refresh_ms),
            move || {
                compute_runtime
                    .read_plugin_panel(&compute_namespace, &compute_panel)
                    .map(bounded_panel_response)
            },
        )
        .await
    {
        Ok(body) => Json((*body).clone()).into_response(),
        Err(orbit_core::OrbitError::NotFound { id, .. }) => not_found(id),
        Err(error) => super::map_runtime_error(error),
    }
}

fn bounded_panel_response(output: Value) -> Value {
    let serialized = serde_json::to_string(&output).unwrap_or_else(|_| "null".to_string());
    let original_bytes = serialized.len();
    if original_bytes <= PANEL_OUTPUT_LIMIT_BYTES {
        return json!({ "output": output });
    }

    // JSON string escaping can expand the preview, so retain at most half the
    // ceiling and leave room for the diagnostic envelope.
    let mut end = PANEL_OUTPUT_LIMIT_BYTES / 2;
    while !serialized.is_char_boundary(end) {
        end -= 1;
    }
    json!({
        "output": &serialized[..end],
        "truncated": true,
        "diagnostic": format!(
            "Panel output was {original_bytes} bytes, above the {PANEL_OUTPUT_LIMIT_BYTES}-byte limit; showing a serialized JSON prefix."
        ),
    })
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
                "granted_roots": permission.granted_roots,
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
                "refresh_ms": panel.refresh_ms,
            }))
            .collect::<Vec<_>>(),
        "links": summary
            .links
            .iter()
            .map(|link| json!({ "title": link.title, "url": link.url }))
            .collect::<Vec<_>>(),
    })
}
