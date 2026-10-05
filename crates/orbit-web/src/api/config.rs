//! Configuration inspection and editing for the dashboard's Config tab
//! [ORB-12724].
//!
//! Reads project `orbit_core::application::config`, which in turn projects
//! what `orbit-config` resolved: these handlers never re-derive a value's
//! layer, section, or description. Writes go through the same admission path
//! as `orbit config set`, so a refused value comes back with the CLI's own
//! message for the row to render verbatim, and every accepted write records a
//! `config.set` audit event naming the key, both values, and the file.

use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Path as UrlPath, Query, State};
use axum::response::{IntoResponse, Json, Response};
use orbit_common::governance::authorization::DASHBOARD_CONFIG_SET;
use orbit_core::OrbitRuntime;
use orbit_core::application::config::{
    ConfigScope, ConfigWriteInit, ConfigWriteOutcome, delete_crew, effective_view, file_view,
    key_catalog, parse_config_scope, set_crew, set_key, unset_key,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::routines::{
    action_capability, authorization_denied, authorized_caller, record_operation_audit,
};
use super::{bad_request, blocking, map_runtime_error, server_error};
use crate::state::{DashboardState, Ws};

/// Audit operation name for every config write, accepted or refused.
const CONFIG_AUDIT_OPERATION: &str = "config.set";

#[derive(Debug, Deserialize, Default)]
pub(super) struct ConfigFileQuery {
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ConfigKeyWrite {
    value: Value,
    #[serde(default)]
    scope: Option<String>,
    /// How to initialize a workspace `config.toml` that does not exist yet:
    /// omitted (refuse), `seed-from-global`, or `fresh`. Mirrors the
    /// `orbit config set` flags rather than inventing a second policy.
    #[serde(default)]
    init: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ConfigCrewWrite {
    #[serde(default)]
    fields: Map<String, Value>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    init: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ConfigScopeQuery {
    #[serde(default)]
    scope: Option<String>,
}

/// `GET /api/config/effective` — the layered view for the selected workspace.
pub(super) async fn get_effective_config(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
) -> Response {
    let operator = state.operator_session();
    match blocking("config effective", move || {
        effective_view(&runtime).map(|mut view| {
            annotate_capability(&mut view, operator);
            view
        })
    })
    .await
    {
        Ok(view) => Json(view).into_response(),
        Err(response) => *response,
    }
}

/// `GET /api/config/file?scope=global|workspace` — one file in isolation.
pub(super) async fn get_config_file(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Query(query): Query<ConfigFileQuery>,
) -> Response {
    let scope = match query.scope.as_deref() {
        Some(raw) => match parse_config_scope(raw) {
            Some(scope) => scope,
            None => return bad_request(scope_error(raw)),
        },
        None => ConfigScope::Workspace,
    };
    let operator = state.operator_session();
    match blocking("config file", move || {
        file_view(&runtime, scope).map(|mut view| {
            annotate_capability(&mut view, operator);
            view
        })
    })
    .await
    {
        Ok(view) => Json(view).into_response(),
        Err(response) => *response,
    }
}

/// `GET /api/config/keys` — the settable-key reference.
///
/// Workspace-scoped like the rest of the API so `?workspace=` selection keeps
/// working, even though the registry itself is machine-wide.
pub(super) async fn get_config_keys(Ws(_runtime): Ws) -> Response {
    Json(key_catalog()).into_response()
}

/// `PUT /api/config/keys/:key` — admit and write one key.
pub(super) async fn put_config_key(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    UrlPath(key): UrlPath<String>,
    Json(body): Json<ConfigKeyWrite>,
) -> Response {
    let (scope, init) = match write_target(body.scope.as_deref(), body.init.as_deref()) {
        Ok(target) => target,
        Err(message) => return bad_request(message),
    };
    let arguments = json!({
        "key": key,
        "value": body.value,
        "scope": scope.label(),
    });
    let value = body.value.clone();
    let write_key = key.clone();
    perform_write(state, runtime, key, arguments, move |runtime| {
        set_key(runtime, &write_key, &value, scope, init)
    })
    .await
}

/// `DELETE /api/config/keys/:key` — clear one key from the selected file.
pub(super) async fn delete_config_key(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    UrlPath(key): UrlPath<String>,
    Query(query): Query<ConfigScopeQuery>,
) -> Response {
    let (scope, _) = match write_target(query.scope.as_deref(), None) {
        Ok(target) => target,
        Err(message) => return bad_request(message),
    };
    let arguments = json!({
        "key": key,
        "value": Value::Null,
        "scope": scope.label(),
    });
    let write_key = key.clone();
    perform_write(state, runtime, key, arguments, move |runtime| {
        unset_key(runtime, &write_key, scope)
    })
    .await
}

/// `PUT /api/config/crews/:name` — create or edit one crew table.
pub(super) async fn put_config_crew(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    UrlPath(name): UrlPath<String>,
    Json(body): Json<ConfigCrewWrite>,
) -> Response {
    let (scope, init) = match write_target(body.scope.as_deref(), body.init.as_deref()) {
        Ok(target) => target,
        Err(message) => return bad_request(message),
    };
    let arguments = json!({
        "key": format!("crews.{name}"),
        "value": Value::Object(body.fields.clone()),
        "scope": scope.label(),
    });
    let fields = body.fields.clone();
    let crew = name.clone();
    perform_write(
        state,
        runtime,
        format!("crews.{name}"),
        arguments,
        move |runtime| set_crew(runtime, &crew, &fields, scope, init),
    )
    .await
}

/// `DELETE /api/config/crews/:name` — remove one crew table.
pub(super) async fn delete_config_crew(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    UrlPath(name): UrlPath<String>,
    Query(query): Query<ConfigScopeQuery>,
) -> Response {
    let (scope, _) = match write_target(query.scope.as_deref(), None) {
        Ok(target) => target,
        Err(message) => return bad_request(message),
    };
    let arguments = json!({
        "key": format!("crews.{name}"),
        "value": Value::Null,
        "scope": scope.label(),
    });
    let crew = name.clone();
    perform_write(
        state,
        runtime,
        format!("crews.{name}"),
        arguments,
        move |runtime| delete_crew(runtime, &crew, scope),
    )
    .await
}

/// Authorize, apply, and audit one config write.
///
/// The three outcomes are audited alike — denied, failed, and applied — so a
/// refused edit is as visible in the audit trail as an accepted one.
async fn perform_write<F>(
    state: DashboardState,
    runtime: Arc<OrbitRuntime>,
    target: String,
    arguments: Value,
    write: F,
) -> Response
where
    F: FnOnce(&OrbitRuntime) -> Result<ConfigWriteOutcome, orbit_core::OrbitError> + Send + 'static,
{
    let workspace = runtime
        .workspace_id()
        .unwrap_or_else(|_| runtime.shared_root().display().to_string());
    let caller = match authorized_caller(&DASHBOARD_CONFIG_SET, state.operator_session()) {
        Ok(caller) => caller,
        Err(denial) => {
            record_operation_audit(
                &runtime,
                &workspace,
                CONFIG_AUDIT_OPERATION,
                &target,
                "",
                &arguments,
                None,
                Some(&denial),
                None,
                Instant::now(),
            )
            .await;
            return authorization_denied(denial);
        }
    };
    let started = Instant::now();
    // Not `blocking`: an admission refusal is the payload the row renders, so
    // the typed error has to survive long enough to be audited before it is
    // mapped to its HTTP shape.
    let applied = {
        let runtime = runtime.clone();
        tokio::task::spawn_blocking(move || write(&runtime)).await
    };
    let outcome = match applied {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(error)) => {
            let failure = error.to_string();
            record_operation_audit(
                &runtime,
                &workspace,
                CONFIG_AUDIT_OPERATION,
                &target,
                "",
                &arguments,
                Some(&caller),
                None,
                Some(&failure),
                started,
            )
            .await;
            return map_runtime_error(error);
        }
        Err(join_error) => {
            return server_error(orbit_core::OrbitError::Execution(format!(
                "config write panicked: {join_error}"
            )));
        }
    };
    let mut applied = arguments.clone();
    if let Some(object) = applied.as_object_mut() {
        object.insert("old_value".to_string(), outcome.old_value.clone());
        object.insert("new_value".to_string(), outcome.new_value.clone());
    }
    record_operation_audit(
        &runtime,
        &workspace,
        CONFIG_AUDIT_OPERATION,
        &target,
        "",
        &applied,
        Some(&caller),
        None,
        None,
        started,
    )
    .await;
    Json(json!({
        "key": target,
        "scope": outcome.scope,
        "path": outcome.path.display().to_string(),
        "old_value": outcome.old_value,
        "new_value": outcome.new_value,
        "rows": outcome.rows,
    }))
    .into_response()
}

/// Resolve the target file and its first-write policy.
///
/// The workspace file is the default target: it is the per-user, git-ignored
/// file an operator edits, and the effective view never writes global.
fn write_target(
    scope: Option<&str>,
    init: Option<&str>,
) -> Result<(ConfigScope, ConfigWriteInit), String> {
    let scope = match scope {
        Some(raw) => parse_config_scope(raw).ok_or_else(|| scope_error(raw))?,
        None => ConfigScope::Workspace,
    };
    let init = match init {
        Some(raw) => ConfigWriteInit::parse(raw).ok_or_else(|| {
            format!("init must be one of: require-existing, seed-from-global, fresh (got '{raw}')")
        })?,
        None => ConfigWriteInit::default(),
    };
    Ok((scope, init))
}

fn scope_error(raw: &str) -> String {
    format!("scope must be one of: global, workspace (got '{raw}')")
}

/// Project the same authorization decision the write endpoints enforce, so the
/// tab can render its rows read-only instead of offering an edit that 403s.
fn annotate_capability(view: &mut Value, operator_session: bool) {
    if let Some(object) = view.as_object_mut() {
        object.insert(
            "config_set".to_string(),
            action_capability(&DASHBOARD_CONFIG_SET, operator_session),
        );
    }
}
