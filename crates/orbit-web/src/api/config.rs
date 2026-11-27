//! Configuration inspection and editing for the dashboard's Config tab
//! [ORB-12724].
//!
//! Reads and writes route through `orbit_core::app::config::exec`: these
//! handlers parse HTTP inputs, invoke one `exec` per request, and render HTTP
//! responses.

use std::ffi::OsStr;
use std::sync::{Arc, OnceLock};

use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use orbit_common::governance::authorization::DASHBOARD_CONFIG_SET;
use orbit_core::Core;
use orbit_core::app::config::{
    Command, ConfigScope, ConfigWriteInit, DashboardConfigWrite, DashboardConfigWriteKind,
    DashboardConfigWriteResult, Outcome, exec, parse_config_scope,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::api::helpers;
use crate::state::{ConfigFileWs, DashboardState, WsCore};

use super::routines::action_capability;
use super::{bad_request, blocking, map_runtime_error};

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

#[derive(Debug, Deserialize, Default)]
pub(super) struct ConfigKeyDeleteQuery {
    #[serde(default)]
    scope: Option<String>,
    /// When true, remove only if the admitted current value still equals its
    /// admitted current removal fallback (used by Settings bulk cleanup).
    #[serde(default)]
    preserve_effective: bool,
}

/// `GET /api/config/effective` — the layered view for the selected workspace.
pub(super) async fn get_effective_config(
    State(state): State<DashboardState>,
    WsCore(core): WsCore,
) -> Response {
    let operator = state.operator_session();
    match blocking("config effective", move || {
        let Outcome::View(mut view) = exec(&core, Command::EffectiveView)? else {
            unreachable!("config effective returned an unrelated outcome");
        };
        annotate_capability(&mut view, operator);
        Ok(view)
    })
    .await
    {
        Ok(view) => Json(view).into_response(),
        Err(response) => *response,
    }
}

/// `GET /api/config/file?scope=global|workspace` — one file in isolation.
///
/// Uses the stale-tolerant workspace resolution so a broken layered config
/// stays repairable here (400 naming the file), while fleet and aggregate
/// endpoints report the same breakage as `unavailable` through the strict
/// resolver.
pub(super) async fn get_config_file(
    State(state): State<DashboardState>,
    ConfigFileWs(core): ConfigFileWs,
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
        let Outcome::View(mut view) = exec(&core, Command::FileView { scope })? else {
            unreachable!("config file returned an unrelated outcome");
        };
        annotate_capability(&mut view, operator);
        Ok(view)
    })
    .await
    {
        Ok(view) => Json(view).into_response(),
        Err(response) => *response,
    }
}

/// `GET /api/config/keys` — the registry key reference, including write availability.
///
/// Workspace-scoped like the rest of the API so `?workspace=` selection keeps
/// working, even though the registry itself is machine-wide.
pub(super) async fn get_config_keys(WsCore(core): WsCore) -> Response {
    let catalog = match exec(&core, Command::Keys) {
        Ok(Outcome::Keys(catalog)) => catalog,
        Ok(_) => unreachable!("config keys returned an unrelated outcome"),
        Err(error) => return map_runtime_error(error),
    };
    // The catalog is built once per process, so its serialized bytes are
    // too; a `serde_json::Value` tree always serializes, hence the empty
    // fallback is unreachable.
    static BODY: OnceLock<Vec<u8>> = OnceLock::new();
    let body = BODY.get_or_init(|| serde_json::to_vec(catalog).unwrap_or_default());
    ([(header::CONTENT_TYPE, "application/json")], body.clone()).into_response()
}

/// `PUT /api/config/keys/:key` — admit and write one key.
pub(super) async fn put_config_key(
    State(state): State<DashboardState>,
    WsCore(core): WsCore,
    UrlPath(key): UrlPath<String>,
    Json(body): Json<ConfigKeyWrite>,
) -> Response {
    let (scope, init) = match write_target(body.scope.as_deref(), body.init.as_deref()) {
        Ok(target) => target,
        Err(message) => return bad_request(message),
    };
    apply_dashboard_write(
        state,
        core,
        DashboardConfigWriteKind::SetKey {
            key,
            value: body.value,
            scope,
            init,
        },
    )
    .await
}

/// `DELETE /api/config/keys/:key` — clear one key from the selected file.
///
/// Unconditional by default for explicit single-key removal. When
/// `preserve_effective=true` is set, the write boundary re-resolves the key
/// under the file's write lock and refuses with `409 Conflict` if the current
/// effective value no longer matches an admitted removal fallback.
pub(super) async fn delete_config_key(
    State(state): State<DashboardState>,
    WsCore(core): WsCore,
    UrlPath(key): UrlPath<String>,
    Query(query): Query<ConfigKeyDeleteQuery>,
) -> Response {
    let (scope, _) = match write_target(query.scope.as_deref(), None) {
        Ok(target) => target,
        Err(message) => return bad_request(message),
    };
    let kind = if query.preserve_effective {
        DashboardConfigWriteKind::UnsetKeyPreservingEffective { key, scope }
    } else {
        DashboardConfigWriteKind::UnsetKey { key, scope }
    };
    apply_dashboard_write(state, core, kind).await
}

/// `PUT /api/config/crews/:name` — create or edit one crew table.
pub(super) async fn put_config_crew(
    State(state): State<DashboardState>,
    WsCore(core): WsCore,
    UrlPath(name): UrlPath<String>,
    Json(body): Json<ConfigCrewWrite>,
) -> Response {
    let (scope, init) = match write_target(body.scope.as_deref(), body.init.as_deref()) {
        Ok(target) => target,
        Err(message) => return bad_request(message),
    };
    apply_dashboard_write(
        state,
        core,
        DashboardConfigWriteKind::SetCrew {
            name,
            fields: body.fields,
            scope,
            init,
        },
    )
    .await
}

/// `DELETE /api/config/crews/:name` — remove one crew table.
pub(super) async fn delete_config_crew(
    State(state): State<DashboardState>,
    WsCore(core): WsCore,
    UrlPath(name): UrlPath<String>,
    Query(query): Query<ConfigScopeQuery>,
) -> Response {
    let (scope, _) = match write_target(query.scope.as_deref(), None) {
        Ok(target) => target,
        Err(message) => return bad_request(message),
    };
    apply_dashboard_write(
        state,
        core,
        DashboardConfigWriteKind::DeleteCrew { name, scope },
    )
    .await
}

/// Authorize, apply, and audit one config write through `app::config::exec`.
async fn apply_dashboard_write(
    state: DashboardState,
    core: Arc<Core>,
    kind: DashboardConfigWriteKind,
) -> Response {
    let write = DashboardConfigWrite {
        kind,
        host: state
            .env()
            .hostname()
            .and_then(OsStr::to_str)
            .map(str::to_string),
    };
    let applied = helpers::blocking_join("config write", move || {
        exec(&core, Command::DashboardWrite(write))
    })
    .await;
    match applied {
        Ok(Ok(Outcome::DashboardWritten(DashboardConfigWriteResult::Applied {
            target,
            outcome,
        }))) => Json(json!({
            "key": target,
            "scope": outcome.scope,
            "path": outcome.path.display().to_string(),
            "old_value": outcome.old_value,
            "new_value": outcome.new_value,
            "rows": outcome.rows,
        }))
        .into_response(),
        Ok(Ok(Outcome::DashboardWritten(DashboardConfigWriteResult::Refused(refusal)))) => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": refusal.message,
                "code": refusal.code,
            })),
        )
            .into_response(),
        Ok(Ok(_)) => unreachable!("dashboard config write returned an unrelated outcome"),
        Ok(Err(error)) => map_runtime_error(error),
        Err(response) => *response,
    }
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
