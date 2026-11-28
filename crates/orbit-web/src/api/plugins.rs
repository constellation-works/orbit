//! Installed plugins and their dashboard panels (design §4.7).
//!
//! Plugin reads and operator-only lifecycle writes:
//!
//! * `GET /api/plugins` — every installed or pinned plugin with its enable
//!   state, diagnostics, tools, panels and link tiles.
//! * `GET /api/plugins/<ns>/panels/<id>` — the JSON one declared panel's
//!   source tool returns.
//! * `POST /api/plugins/<ns>/{enable,disable}` — toggle host or workspace.
//! * `GET /api/plugins/<ns>/pages/<page>/` — an active page as one document
//!   under its frame policy, served only to same-origin dashboard iframe
//!   navigations (plugin-pages design §2).
//! * `GET /api/plugins/<ns>/pages/<page>/parts` — the same page's markup,
//!   scripts, styles and kit version as JSON, from which another host's
//!   dashboard writes the document itself (plugin-pages design §5).
//! * `POST /api/plugins/<ns>/pages/<page>/call` — one call the page makes
//!   to a `read_only` tool it declares (plugin-pages design §3).
//! * `POST /api/plugins/<ns>/pages/<page>/actions/confirm` — check one
//!   action the page asks for and issue the one-time confirmation the
//!   dashboard's dialog sends back; nothing runs.
//! * `POST /api/plugins/<ns>/pages/<page>/actions/run` — run that action
//!   once, with the confirmation, after the operator confirmed it.
//! * `POST /api/plugins/<ns>/pages/<page>/actions/cancel` — release a
//!   confirmation the operator did not use; nothing runs.
//!
//! A panel source is always a `read_only` tool: the manifest cannot declare
//! a panel over a mutating one (`orbit plugin validate` refuses it, naming
//! the panel), and the read re-checks the loaded manifest before executing.
//! That is what makes a panel safe to serve to any dashboard session while
//! the dashboard's mutations still require an operator session. A page
//! call is held to the same rule, plus the page's own `tools` list and the
//! tool's input schema, all checked before anything runs. An action needs
//! the operator session at both steps, and the run spends a confirmation
//! issued for exactly its plugin, page, tool and input; every run request,
//! refused or run, has an audit row carrying the web session.

use std::ffi::OsStr;
use std::path::Path as FsPath;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use orbit_common::OrbitError;
use orbit_common::governance::authorization::{
    DASHBOARD_PLUGIN_DISABLE, DASHBOARD_PLUGIN_ENABLE, DASHBOARD_PLUGIN_PAGE_ACTION,
};
use orbit_core::Core;
use orbit_core::app::plugin::{
    Command, DashboardScope, DashboardToggle, Outcome, PageActionRun, exec,
};
use orbit_core::app::plugin::{PluginSummary, PluginSummaryJsonProfile, plugin_summary_json};
use serde_json::{Value, json};

use super::routines::{action_capability, authorization_denied, authorized_caller};
use super::{blocking, not_found, validate_id};
use crate::plugin_page::{ActionBinding, CallRefused, ConfirmationRefused};
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
                orbit_common::fs::path::home_dir(&orbit_core::app::host::composition_env()).ok();
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
                orbit_common::fs::path::home_dir(&orbit_core::app::host::composition_env()).ok();
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

fn plugin_write_error(error: orbit_common::OrbitError) -> Response {
    match error {
        orbit_common::OrbitError::AuthorizationDenied(denial) => authorization_denied(*denial),
        OrbitError::NotFound { id, .. } => not_found(format!("plugin not found: {id}")),
        orbit_common::OrbitError::PluginDisabledOnHost { plugin } => (
            StatusCode::CONFLICT,
            Json(json!({"code": "PluginDisabledOnHost", "error": format!("plugin '{plugin}' is disabled on this host; enable it on the host first")})),
        ).into_response(),
        orbit_common::OrbitError::PolicyDenied(message) => (
            StatusCode::CONFLICT,
            Json(json!({"code": "plugin_refused", "error": message})),
        ).into_response(),
        orbit_common::OrbitError::InvalidInput(message) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"code": "plugin_refused", "error": message})),
        ).into_response(),
        orbit_common::OrbitError::InvalidInputDiagnostic { message, .. } => (
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
        Err(orbit_common::OrbitError::NotFound { id, .. }) => not_found(id),
        Err(error) => super::map_runtime_error(error),
    }
}

pub(super) async fn read_page(
    State(state): State<DashboardState>,
    workspace: PluginPanelWs,
    headers: HeaderMap,
    Path((namespace, page)): Path<(String, String)>,
) -> Response {
    if let Err(reason) = crate::plugin_page::require_dashboard_frame_navigation(&headers) {
        return frame_required(reason);
    }
    let source = match active_page(&workspace, namespace, page).await {
        Ok(source) => source,
        Err(response) => return *response,
    };
    page_document_response(&state.plugin_pages().document(&source))
}

/// `GET /api/plugins/<ns>/pages/<page>/parts`: the parts another host's
/// dashboard writes this page's document from (plugin-pages design §5). It
/// answers for exactly the pages the document route serves. The bytes are
/// data here; only the dashboard that writes the document runs them, under
/// its own policy.
pub(super) async fn read_page_parts(
    workspace: PluginPanelWs,
    Path((namespace, page)): Path<(String, String)>,
) -> Response {
    let source = match active_page(&workspace, namespace, page).await {
        Ok(source) => source,
        Err(response) => return *response,
    };
    let mut response = Json(crate::plugin_page::parts_json(&source)).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// The active page `page` of `namespace`, or the response that says why it
/// is not served: a malformed id, or a 404 for an unknown page or a plugin
/// that is inactive or disabled in this workspace.
async fn active_page(
    workspace: &PluginPanelWs,
    namespace: String,
    page: String,
) -> Result<orbit_core::app::plugin::PageSource, Box<Response>> {
    for (label, value) in [("plugin", &namespace), ("page", &page)] {
        if let Err(message) = validate_id(value) {
            return Err(Box::new(super::bad_request(format!("{label} {message}"))));
        }
    }
    let core = workspace.core.clone();
    match tokio::task::spawn_blocking(move || exec(&core, Command::ReadPage { namespace, page }))
        .await
    {
        Ok(Ok(Outcome::Page(source))) => Ok(*source),
        Ok(Ok(_)) => unreachable!("read plugin page returned an unrelated outcome"),
        Ok(Err(error)) => Err(Box::new(page_error(&error))),
        Err(error) => Err(Box::new(page_panicked(error))),
    }
}

/// The 403 a page document request gets without Fetch Metadata proving a
/// same-origin dashboard iframe navigation.
pub(super) fn frame_required(reason: &str) -> Response {
    let mut response = page_refusal(StatusCode::FORBIDDEN, "frame_required", reason.to_string());
    let response_headers = response.headers_mut();
    response_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response_headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'"),
    );
    response
}

/// A written page document with its frame policy, for a local page and for
/// one this dashboard wrote from another host's parts.
pub(super) fn page_document_response(document: &crate::plugin_page::PageDocument) -> Response {
    let mut response = document.body.clone().into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    match HeaderValue::from_str(&document.policy) {
        Ok(policy) => {
            headers.insert(header::CONTENT_SECURITY_POLICY, policy);
        }
        // The policy is ASCII Orbit wrote; without it the page is not served.
        Err(error) => {
            return super::server_error(OrbitError::Execution(format!(
                "plugin page policy is not a header value: {error}"
            )));
        }
    }
    response
}

pub(super) async fn call_page(
    State(state): State<DashboardState>,
    workspace: PluginPanelWs,
    Path((namespace, page)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Response {
    for (label, value) in [("plugin", &namespace), ("page", &page)] {
        if let Err(message) = validate_id(value) {
            return super::bad_request(format!("{label} {message}"));
        }
    }
    let Some(tool) = body.get("tool").and_then(Value::as_str).map(str::to_string) else {
        return page_refusal(
            StatusCode::BAD_REQUEST,
            "input_invalid",
            "a page call names its `tool`".to_string(),
        );
    };
    let input = match body.get("input") {
        None | Some(Value::Null) => Value::Object(Default::default()),
        Some(input) => input.clone(),
    };
    let core = workspace.core.clone();
    let checked = {
        let (namespace, page) = (namespace.clone(), page.clone());
        tokio::task::spawn_blocking(move || {
            exec(
                &core,
                Command::CallPage {
                    namespace,
                    page,
                    tool,
                    input,
                },
            )
        })
        .await
    };
    let call = match checked {
        Ok(Ok(Outcome::PageCall(call))) => *call,
        Ok(Ok(_)) => unreachable!("plugin page call returned an unrelated outcome"),
        Ok(Err(error)) => return page_error(&error),
        Err(error) => return page_panicked(error),
    };
    match state
        .plugin_pages()
        .call(workspace.runtime_identity(), &namespace, &page, call)
        .await
    {
        Ok(Ok(output)) => Json(bounded_panel_response((*output).clone())).into_response(),
        Ok(Err(error)) => page_error(&error),
        Err(CallRefused::Busy) => page_refusal(
            StatusCode::TOO_MANY_REQUESTS,
            "busy",
            format!(
                "page '{page}' of plugin '{namespace}' already has {} calls running; try again \
                 when one finishes",
                crate::plugin_page::PAGE_CALL_CONCURRENCY
            ),
        ),
    }
}

/// The tool and input a page action request names.
fn action_request(body: &Value) -> Result<(String, Value), Box<Response>> {
    let Some(tool) = body.get("tool").and_then(Value::as_str).map(str::to_string) else {
        return Err(Box::new(page_refusal(
            StatusCode::BAD_REQUEST,
            "input_invalid",
            "a page action names its `tool`".to_string(),
        )));
    };
    let input = match body.get("input") {
        None | Some(Value::Null) => Value::Object(Default::default()),
        Some(input) => input.clone(),
    };
    Ok((tool, input))
}

pub(super) async fn confirm_page_action(
    State(state): State<DashboardState>,
    workspace: PluginPanelWs,
    Path((namespace, page)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Response {
    for (label, value) in [("plugin", &namespace), ("page", &page)] {
        if let Err(message) = validate_id(value) {
            return super::bad_request(format!("{label} {message}"));
        }
    }
    let (tool, input) = match action_request(&body) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    let core = workspace.core.clone();
    let checked = {
        let (namespace, page, tool, input) =
            (namespace.clone(), page.clone(), tool.clone(), input.clone());
        tokio::task::spawn_blocking(move || {
            exec(
                &core,
                Command::CheckPageAction {
                    namespace,
                    page,
                    tool,
                    input,
                },
            )
        })
        .await
    };
    let action = match checked {
        Ok(Ok(Outcome::PageAction(action))) => *action,
        Ok(Ok(_)) => unreachable!("plugin page action check returned an unrelated outcome"),
        Ok(Err(OrbitError::AuthorizationDenied(denial))) => return authorization_denied(*denial),
        Ok(Err(error)) => return page_error(&error),
        Err(error) => return page_panicked(error),
    };
    let binding = ActionBinding {
        runtime: workspace.runtime_identity(),
        namespace,
        page,
        tool,
        input,
    };
    match state.plugin_pages().issue_confirmation(binding) {
        Ok(confirmation) => Json(json!({
            "confirmation": confirmation,
            "expires_in_ms": crate::plugin_page::ACTION_CONFIRMATION_TTL.as_millis(),
            "plugin": action.plugin,
            "version": action.version,
            "page": action.page,
            "page_title": action.page_title,
            "tool": action.tool,
            "description": action.description,
            "input": action.input,
        }))
        .into_response(),
        Err(ConfirmationRefused::Busy) => page_refusal(
            StatusCode::TOO_MANY_REQUESTS,
            "busy",
            format!(
                "{} action confirmations are waiting for the operator; answer one first",
                crate::plugin_page::ACTION_CONFIRMATION_LIMIT
            ),
        ),
        Err(ConfirmationRefused::Unavailable(message)) => super::server_error(
            OrbitError::Execution(format!("action confirmation: {message}")),
        ),
    }
}

/// Release a confirmation the operator did not use (Cancel, Escape, a frame
/// that went away, or another dialog already open), so the slot is free at
/// once. Idempotent: a token that is unknown, spent, expired or issued for
/// another request answers `released: false`. Nothing runs and no audit row
/// is written, as for the check that issued it.
pub(super) async fn cancel_page_action(
    State(state): State<DashboardState>,
    workspace: PluginPanelWs,
    Path((namespace, page)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Response {
    for (label, value) in [("plugin", &namespace), ("page", &page)] {
        if let Err(message) = validate_id(value) {
            return super::bad_request(format!("{label} {message}"));
        }
    }
    if let Err(denial) = authorized_caller(&DASHBOARD_PLUGIN_PAGE_ACTION, state.operator_session())
    {
        return authorization_denied(denial);
    }
    let (tool, input) = match action_request(&body) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    let released = body
        .get("confirmation")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .is_some_and(|token| {
            state.plugin_pages().release_confirmation(
                token,
                &ActionBinding {
                    runtime: workspace.runtime_identity(),
                    namespace,
                    page,
                    tool,
                    input,
                },
            )
        });
    Json(json!({"released": released})).into_response()
}

pub(super) async fn run_page_action(
    State(state): State<DashboardState>,
    workspace: PluginPanelWs,
    Path((namespace, page)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Response {
    for (label, value) in [("plugin", &namespace), ("page", &page)] {
        if let Err(message) = validate_id(value) {
            return super::bad_request(format!("{label} {message}"));
        }
    }
    let (tool, input) = match action_request(&body) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    // Spent before anything else, so a confirmation is good for one request
    // whatever that request turns out to be.
    let confirmation = state.plugin_pages().redeem_confirmation(
        body.get("confirmation").and_then(Value::as_str),
        &ActionBinding {
            runtime: workspace.runtime_identity(),
            namespace: namespace.clone(),
            page: page.clone(),
            tool: tool.clone(),
            input: input.clone(),
        },
    );
    let confirmed = confirmation.is_ok();
    let run = PageActionRun {
        namespace,
        page,
        tool,
        input,
        confirmation,
        session: Some(state.web_session_id().to_string()),
        host: state
            .env()
            .hostname()
            .and_then(OsStr::to_str)
            .map(str::to_string),
    };
    let core = workspace.core.clone();
    let result =
        tokio::task::spawn_blocking(move || exec(&core, Command::RunPageAction(Box::new(run))))
            .await;
    match result {
        Ok(Ok(Outcome::ToolOutput(output))) => Json(bounded_panel_response(output)).into_response(),
        Ok(Ok(_)) => unreachable!("plugin page action returned an unrelated outcome"),
        Ok(Err(OrbitError::AuthorizationDenied(denial))) => authorization_denied(*denial),
        Ok(Err(OrbitError::PolicyDenied(message))) if !confirmed => {
            page_refusal(StatusCode::FORBIDDEN, "confirmation_refused", message)
        }
        Ok(Err(error)) => page_error(&error),
        Err(error) => page_panicked(error),
    }
}

fn page_panicked(error: tokio::task::JoinError) -> Response {
    super::server_error(OrbitError::Execution(format!(
        "plugin page request panicked: {error}"
    )))
}

fn page_refusal(status: StatusCode, code: &str, message: String) -> Response {
    (status, Json(json!({"code": code, "error": message}))).into_response()
}

/// A page read or call failure. A page that is not served is a 404 with its
/// reason; a refused tool is a 403 and input that does not fit a 400, and
/// in both cases nothing ran.
fn page_error(error: &OrbitError) -> Response {
    match error {
        OrbitError::NotFound { id, .. } => not_found(id.clone()),
        OrbitError::PolicyDenied(message) => {
            page_refusal(StatusCode::FORBIDDEN, "tool_refused", message.clone())
        }
        OrbitError::InvalidInput(message) => {
            page_refusal(StatusCode::BAD_REQUEST, "input_invalid", message.clone())
        }
        other => page_refusal(
            StatusCode::INTERNAL_SERVER_ERROR,
            "call_failed",
            other.to_string(),
        ),
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
    plugin_summary_json(
        summary,
        PluginSummaryJsonProfile::Http {
            home,
            host_orbit_version: env!("CARGO_PKG_VERSION"),
        },
    )
}
