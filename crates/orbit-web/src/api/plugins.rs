//! Installed plugins and their dashboard panels (design §4.7).
//!
//! Plugin reads and operator-only lifecycle writes:
//!
//! * `GET /api/plugins` — every installed or pinned plugin with its enable
//!   state, diagnostics, tools, panels and link tiles.
//! * `GET /api/plugins/<ns>/panels/<id>` — the JSON one declared panel's
//!   source tool returns.
//! * `POST /api/plugins/<ns>/{enable,disable}` — toggle host or workspace.
//!
//! A panel source is always a `read_only` tool: the manifest cannot declare
//! a panel over a mutating one (`orbit plugin validate` refuses it, naming
//! the panel), and the read re-checks the loaded manifest before executing.
//! That is what makes a panel safe to serve to any dashboard session while
//! the dashboard's mutations still require an operator session.

use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use orbit_common::governance::authorization::{
    DASHBOARD_PLUGIN_DISABLE, DASHBOARD_PLUGIN_ENABLE, GovernedOperation,
};
use orbit_core::adapter::command::PluginSummary;
use orbit_types::plugin::PluginStatus;
use serde_json::{Value, json};

use super::routines::{
    action_capability, authorization_denied, authorized_caller, record_operation_audit,
};
use super::{blocking, not_found, validate_id};
use crate::state::{DashboardState, Ws};

/// Maximum serialized tool output retained or returned for one panel.
pub(crate) const PANEL_OUTPUT_LIMIT_BYTES: usize = 256 * 1024;

pub(super) async fn list_plugins(State(state): State<DashboardState>, Ws(runtime): Ws) -> Response {
    let operator = state.operator_session();
    match blocking("list plugins", move || runtime.list_plugins()).await {
        Ok(plugins) => Json(Value::Array(
            plugins
                .iter()
                .map(|plugin| {
                    let mut value = plugin_to_json(plugin);
                    value["capabilities"] = json!({
                        "enable": action_capability(&DASHBOARD_PLUGIN_ENABLE, operator),
                        "disable": action_capability(&DASHBOARD_PLUGIN_DISABLE, operator),
                    });
                    value
                })
                .collect(),
        ))
        .into_response(),
        Err(response) => *response,
    }
}

pub(super) async fn enable_plugin(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Path(namespace): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    mutate_plugin(state, runtime, namespace, body, true).await
}

pub(super) async fn disable_plugin(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Path(namespace): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    mutate_plugin(state, runtime, namespace, body, false).await
}

async fn mutate_plugin(
    state: DashboardState,
    runtime: Arc<orbit_core::OrbitRuntime>,
    namespace: String,
    body: Value,
    enable: bool,
) -> Response {
    let operation: &'static GovernedOperation = if enable {
        &DASHBOARD_PLUGIN_ENABLE
    } else {
        &DASHBOARD_PLUGIN_DISABLE
    };
    let started = Instant::now();
    let workspace = runtime
        .workspace_id()
        .unwrap_or_else(|_| runtime.shared_root().display().to_string());
    let caller = match authorized_caller(operation, state.operator_session()) {
        Ok(caller) => caller,
        Err(denial) => {
            record_operation_audit(
                &runtime,
                &workspace,
                operation.id,
                &namespace,
                "",
                &body,
                None,
                Some(&denial),
                None,
                started,
            )
            .await;
            return authorization_denied(denial);
        }
    };
    let result = if let Err(message) = validate_id(&namespace) {
        Err(orbit_core::OrbitError::InvalidInput(format!(
            "plugin {message}"
        )))
    } else {
        match body.get("scope").and_then(Value::as_str) {
            Some("host" | "workspace") => {
                let scope = body["scope"].as_str().unwrap_or_default().to_string();
                let runtime_for_write = Arc::clone(&runtime);
                let name = namespace.clone();
                match tokio::task::spawn_blocking(move || {
                    if !runtime_for_write
                        .list_plugins()?
                        .iter()
                        .any(|plugin| plugin.name == name)
                    {
                        return Ok(None);
                    }
                    let summary = match (enable, scope.as_str()) {
                        (true, "host") => {
                            runtime_for_write
                                .enable_plugin_from_dashboard(&name)?
                                .summary
                        }
                        (false, "host") => runtime_for_write.disable_plugin(&name)?,
                        (true, _) => {
                            runtime_for_write
                                .enable_plugin_in_workspace(&name, false)?
                                .summary
                        }
                        (false, _) => runtime_for_write.disable_plugin_in_workspace(&name)?,
                    };
                    Ok(Some(summary))
                })
                .await
                {
                    Ok(result) => result,
                    Err(error) => Err(orbit_core::OrbitError::Execution(format!(
                        "plugin write panicked: {error}"
                    ))),
                }
            }
            _ => Err(orbit_core::OrbitError::InvalidInput(
                "scope must be `workspace` or `host`".to_string(),
            )),
        }
    };
    match result {
        Ok(Some(summary)) => {
            record_operation_audit(
                &runtime,
                &workspace,
                operation.id,
                &namespace,
                "",
                &body,
                Some(&caller),
                None,
                None,
                started,
            )
            .await;
            Json(json!({"plugin": plugin_to_json(&summary)})).into_response()
        }
        Ok(None) => {
            let failure = format!("plugin not found: {namespace}");
            record_operation_audit(
                &runtime,
                &workspace,
                operation.id,
                &namespace,
                "",
                &body,
                Some(&caller),
                None,
                Some(&failure),
                started,
            )
            .await;
            not_found(failure)
        }
        Err(error) => {
            let failure = error.to_string();
            record_operation_audit(
                &runtime,
                &workspace,
                operation.id,
                &namespace,
                "",
                &body,
                Some(&caller),
                None,
                Some(&failure),
                started,
            )
            .await;
            plugin_write_error(error)
        }
    }
}

fn plugin_write_error(error: orbit_core::OrbitError) -> Response {
    match error {
        orbit_core::OrbitError::NotFound { id, .. } => not_found(format!("plugin not found: {id}")),
        orbit_core::OrbitError::PluginDisabledOnHost { plugin } => (
            StatusCode::CONFLICT,
            Json(json!({"code": "PluginDisabledOnHost", "error": format!("plugin '{plugin}' is disabled on this host; enable it on the host first")})),
        ).into_response(),
        orbit_core::OrbitError::PolicyDenied(message) => (
            StatusCode::CONFLICT,
            Json(json!({"code": "plugin_refused", "error": message})),
        ).into_response(),
        orbit_core::OrbitError::InvalidInput(message) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"code": "plugin_refused", "error": message})),
        ).into_response(),
        orbit_core::OrbitError::InvalidInputDiagnostic { message, .. } => (
            StatusCode::BAD_REQUEST,
            Json(json!({"code": "plugin_refused", "error": message})),
        ).into_response(),
        other => super::map_runtime_error(other),
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
        "host_enabled": summary.host_enabled,
        "workspace_toggle": summary.workspace_toggle,
        "disabled_by": summary.disabled_by.map(|layer| layer.as_str()),
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
