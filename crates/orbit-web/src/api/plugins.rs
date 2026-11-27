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

use std::ffi::OsStr;
use std::path::Path as FsPath;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use orbit_common::governance::authorization::{DASHBOARD_PLUGIN_DISABLE, DASHBOARD_PLUGIN_ENABLE};
use orbit_core::adapter::command::{self, PluginSummary};
use orbit_core::app::plugin::{Command, DashboardScope, DashboardToggle, Outcome, exec};
use orbit_core::{Core, OrbitError};
use serde_json::{Value, json};

use super::routines::{action_capability, authorization_denied};
use super::{blocking, not_found, validate_id};
use crate::state::{DashboardState, PluginPanelWs, WsCore};

/// Maximum serialized tool output retained or returned for one panel.
pub(crate) const PANEL_OUTPUT_LIMIT_BYTES: usize = 256 * 1024;

pub(super) async fn list_plugins(
    State(state): State<DashboardState>,
    WsCore(core): WsCore,
) -> Response {
    let operator = state.operator_session();
    match blocking("list plugins", move || exec(&core, Command::List)).await {
        Ok(Outcome::Listed(plugins)) => Json(Value::Array({
            let home =
                orbit_common::fs::path::home_dir(&orbit_core::composition::composition_env()).ok();
            plugins
                .iter()
                .map(|plugin| {
                    let mut value = plugin_to_json(plugin, home.as_deref());
                    value["capabilities"] = json!({
                        "enable": action_capability(&DASHBOARD_PLUGIN_ENABLE, operator),
                        "disable": action_capability(&DASHBOARD_PLUGIN_DISABLE, operator),
                    });
                    value
                })
                .collect()
        }))
        .into_response(),
        Ok(_) => unreachable!("list plugins returned an unrelated outcome"),
        Err(response) => *response,
    }
}

pub(super) async fn enable_plugin(
    State(state): State<DashboardState>,
    WsCore(core): WsCore,
    Path(namespace): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    mutate_plugin(state, core, namespace, body, true).await
}

pub(super) async fn disable_plugin(
    State(state): State<DashboardState>,
    WsCore(core): WsCore,
    Path(namespace): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    mutate_plugin(state, core, namespace, body, false).await
}

async fn mutate_plugin(
    state: DashboardState,
    core: std::sync::Arc<Core>,
    namespace: String,
    body: Value,
    enable: bool,
) -> Response {
    let scope = validate_id(&namespace)
        .map_err(|message| OrbitError::InvalidInput(format!("plugin {message}")))
        .and_then(|_| match body.get("scope").and_then(Value::as_str) {
            Some("host") => Ok(DashboardScope::Host),
            Some("workspace") => Ok(DashboardScope::Workspace),
            _ => Err(OrbitError::InvalidInput(
                "scope must be `workspace` or `host`".to_string(),
            )),
        });
    let toggle = DashboardToggle {
        namespace: namespace.clone(),
        scope,
        enable,
        arguments: body,
        host: state
            .env()
            .hostname()
            .and_then(OsStr::to_str)
            .map(str::to_string),
    };
    let result =
        tokio::task::spawn_blocking(move || exec(&core, Command::DashboardToggle(toggle))).await;
    match result {
        Ok(Ok(Outcome::DashboardToggled(Some(summary)))) => {
            let home =
                orbit_common::fs::path::home_dir(&orbit_core::composition::composition_env()).ok();
            Json(json!({"plugin": plugin_to_json(&summary, home.as_deref())})).into_response()
        }
        Ok(Ok(Outcome::DashboardToggled(None))) => {
            not_found(format!("plugin not found: {namespace}"))
        }
        Ok(Ok(_)) => unreachable!("dashboard plugin write returned an unrelated outcome"),
        Ok(Err(error)) => plugin_write_error(error),
        Err(error) => plugin_write_error(OrbitError::Execution(format!(
            "plugin write panicked: {error}"
        ))),
    }
}

fn plugin_write_error(error: orbit_core::OrbitError) -> Response {
    match error {
        orbit_core::OrbitError::AuthorizationDenied(denial) => authorization_denied(*denial),
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
    workspace: PluginPanelWs,
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
    let read = match exec(
        &workspace.core,
        Command::ReadPanel {
            namespace: namespace.clone(),
            panel: panel.clone(),
        },
    ) {
        Ok(Outcome::Panel(read)) => read,
        Ok(_) => unreachable!("read plugin panel returned an unrelated outcome"),
        Err(OrbitError::NotFound { id, .. }) => return not_found(id),
        Err(error) => return super::map_runtime_error(error),
    };
    match workspace
        .read_cached(&state, (namespace, panel), read.ttl(), move || {
            read.read().map(bounded_panel_response)
        })
        .await
    {
        Ok(served) => Json((*served.body).clone()).into_response(),
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

/// Projects one plugin for the dashboard. `source`, `install_path`, and the
/// `programs[]` `path` and `resolves_to` are display-only and can be absolute
/// host paths, so they are `~`-abbreviated like the workspace listing rather
/// than disclosing the home directory.
pub(super) fn plugin_to_json(summary: &PluginSummary, home: Option<&FsPath>) -> Value {
    command::plugin_summary_json(
        summary,
        command::PluginSummaryJsonProfile::Http {
            home,
            host_orbit_version: env!("CARGO_PKG_VERSION"),
        },
    )
}
