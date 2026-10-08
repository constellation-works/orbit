use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use orbit_common::governance::authorization::DASHBOARD_HOST_EDIT;
use orbit_common::{HostRegistryCode, OrbitError};
use orbit_registry::hosts::validated_host_root;
use serde::Deserialize;
use serde_json::{Value, json};

use super::blocking;
use super::routines::{action_capability, authorization_denied, authorized_caller};
use crate::state::DashboardState;

/// Always reports the serving host, without a workspace extractor or remote routing.
pub(super) async fn resources(State(state): State<DashboardState>) -> Response {
    match blocking("host resource sampling", move || {
        state.host_resource_status()
    })
    .await
    {
        Ok(status) => Json(status).into_response(),
        Err(response) => *response,
    }
}

// Settings › Hosts [ORB-14451]. Every route calls the operation `orbit host`
// calls, on the serving host's host file, and answers with the CLI's JSON
// shape and error codes. Probes and file reads run on the blocking pool, and
// each probe has its own federated budget, so an unreachable host never holds
// a worker or another panel. Mutations pass the router's origin guard and are
// governed: like a config write, a host-file edit needs the operator session.

#[derive(Debug, Deserialize)]
pub(super) struct HostListQuery {
    /// `false` lists cached fields only and opens no session.
    #[serde(default = "probe_by_default")]
    probe: bool,
}

fn probe_by_default() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub(super) struct HostRemoveQuery {
    #[serde(default)]
    force: bool,
}

/// `orbit host add <ssh> [--name <name>]`. Nothing else is accepted: no
/// command, and no ssh options.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HostAddBody {
    ssh: String,
    #[serde(default)]
    name: Option<String>,
}

/// `orbit host rename <host> <name>`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HostRenameBody {
    name: String,
}

/// `GET /api/hosts[?probe=false]`: `orbit host list --json`, read from the
/// last valid host file, plus the error of a newer file that failed to load
/// and whether this session may edit it.
pub(super) async fn list_hosts(
    State(state): State<DashboardState>,
    Query(query): Query<HostListQuery>,
) -> Response {
    host_blocking("host list", move || {
        let pinned = state.hosts();
        let Some(snapshot) = pinned.snapshot else {
            return Ok(match pinned.load_error {
                Some(error) => failure(&error.code, error.message, None),
                None => failure("internal_error", "host file not loaded".into(), None),
            });
        };
        let list = orbit_cmd::hosts::list_registered_hosts(&snapshot.registry, query.probe)?;
        let mut body = to_json(&list)?;
        if let Some(object) = body.as_object_mut() {
            object.insert("generation".into(), json!(snapshot.generation));
            object.insert("load_error".into(), json!(pinned.load_error));
            object.insert(
                "host_edit".into(),
                action_capability(&DASHBOARD_HOST_EDIT, state.operator_session()),
            );
        }
        Ok(Json(body).into_response())
    })
    .await
}

/// `GET /api/hosts/:host`: `orbit host show --json`, dependents included.
pub(super) async fn show_host(
    State(state): State<DashboardState>,
    UrlPath(host): UrlPath<String>,
) -> Response {
    host_blocking("host show", move || {
        let pinned = state.hosts();
        let Some(snapshot) = pinned.snapshot else {
            return Ok(match pinned.load_error {
                Some(error) => failure(&error.code, error.message, None),
                None => failure("internal_error", "host file not loaded".into(), None),
            });
        };
        let detail = orbit_cmd::hosts::show_registered_host(&snapshot.registry, &host)?;
        Ok(Json(to_json(&detail)?).into_response())
    })
    .await
}

/// `POST /api/hosts`: `orbit host add`. The ssh target is validated before
/// any process starts and then only ever passed after `--`.
pub(super) async fn add_host(
    State(state): State<DashboardState>,
    Json(body): Json<Value>,
) -> Response {
    if let Err(denial) = authorized_caller(&DASHBOARD_HOST_EDIT, state.operator_session()) {
        return authorization_denied(denial);
    }
    let body: HostAddBody = match parse_body(body) {
        Ok(body) => body,
        Err(response) => return *response,
    };
    let root = state.global_root().to_path_buf();
    host_blocking("host add", move || {
        let root = validated_host_root(&root)?;
        let name = body
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty());
        let change = orbit_cmd::hosts::add_host(&root, &body.ssh, name)?;
        Ok((StatusCode::CREATED, Json(to_json(&change)?)).into_response())
    })
    .await
}

/// `PATCH /api/hosts/:host`: `orbit host rename`.
pub(super) async fn rename_host(
    State(state): State<DashboardState>,
    UrlPath(host): UrlPath<String>,
    Json(body): Json<Value>,
) -> Response {
    if let Err(denial) = authorized_caller(&DASHBOARD_HOST_EDIT, state.operator_session()) {
        return authorization_denied(denial);
    }
    let body: HostRenameBody = match parse_body(body) {
        Ok(body) => body,
        Err(response) => return *response,
    };
    let root = state.global_root().to_path_buf();
    host_blocking("host rename", move || {
        let root = validated_host_root(&root)?;
        let change = orbit_cmd::hosts::rename_host(&root, &host, &body.name)?;
        Ok(Json(to_json(&change)?).into_response())
    })
    .await
}

/// `DELETE /api/hosts/:host[?force=true]`: `orbit host remove [--force]`. A
/// `host_in_use` refusal carries the dependents it names, so the view can
/// list them beside its force confirmation.
pub(super) async fn remove_host(
    State(state): State<DashboardState>,
    UrlPath(host): UrlPath<String>,
    Query(query): Query<HostRemoveQuery>,
) -> Response {
    if let Err(denial) = authorized_caller(&DASHBOARD_HOST_EDIT, state.operator_session()) {
        return authorization_denied(denial);
    }
    let root = state.global_root().to_path_buf();
    host_blocking("host remove", move || {
        let root = validated_host_root(&root)?;
        match orbit_cmd::hosts::remove_host(&root, &host, query.force) {
            Ok(change) => Ok(Json(to_json(&change)?).into_response()),
            Err(error) if error.host_registry_code() == Some(HostRegistryCode::HostInUse) => {
                let dependents = orbit_cmd::hosts::dependents_of(&root, &host)
                    .ok()
                    .flatten()
                    .map(|dependents| to_json(&dependents))
                    .transpose()?;
                Ok(failure(
                    host_error_code(&error),
                    error.to_string(),
                    dependents.map(|dependents| ("dependents", dependents)),
                ))
            }
            Err(error) => Err(error),
        }
    })
    .await
}

/// The CLI's error code for every error a host operation returns, so the
/// dashboard reports `orbit host --json`'s typed refusals verbatim.
pub(crate) fn host_error_code(error: &OrbitError) -> &str {
    match error {
        OrbitError::HostRegistry { code, .. } => code.as_str(),
        OrbitError::InvalidInput(_) | OrbitError::InvalidInputDiagnostic { .. } => "invalid_input",
        OrbitError::UnreachableDestination(_) => "unreachable_destination",
        OrbitError::AmbiguousDestination(_) => "ambiguous_destination",
        OrbitError::OutcomeUnknown { .. } => "outcome_unknown",
        OrbitError::RemoteTool { code, .. } => code.as_str(),
        OrbitError::ProcessTimeout { .. } => "process_timeout",
        OrbitError::Execution(_) => "execution_failed",
        OrbitError::WorkspaceError(_) => "workspace_error",
        OrbitError::Io(_) => "io_error",
        _ => "internal_error",
    }
}

pub(super) fn status_for(code: &str) -> StatusCode {
    match code {
        "invalid_input" => StatusCode::BAD_REQUEST,
        "unknown_host" => StatusCode::NOT_FOUND,
        "host_exists"
        | "host_name_conflict"
        | "task_prefix_conflict"
        | "host_is_local"
        | "host_in_use"
        | "host_file_conflict"
        | "host_identity_mismatch"
        | "host_too_old"
        | "ambiguous_destination" => StatusCode::CONFLICT,
        "unreachable_destination"
        | "legacy_host_unreachable"
        | "outcome_unknown"
        | "process_timeout" => StatusCode::BAD_GATEWAY,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// `{error, code}`, the CLI's `--json` error object, with the status its code implies.
fn failure(code: &str, message: String, extra: Option<(&str, Value)>) -> Response {
    let mut body = json!({ "error": message, "code": code });
    if let (Some((key, value)), Some(object)) = (extra, body.as_object_mut()) {
        object.insert(key.to_string(), value);
    }
    (status_for(code), Json(body)).into_response()
}

/// A body with a missing, mistyped or unknown field is the CLI's
/// `invalid_input`, not the framework's bare 422.
fn parse_body<T: serde::de::DeserializeOwned>(body: Value) -> Result<T, Box<Response>> {
    serde_json::from_value(body).map_err(|error| {
        Box::new(failure(
            "invalid_input",
            format!("invalid request body: {error}"),
            None,
        ))
    })
}

fn to_json(value: &impl serde::Serialize) -> Result<Value, OrbitError> {
    serde_json::to_value(value)
        .map_err(|error| OrbitError::Execution(format!("serialize host report: {error}")))
}

/// Run a host operation on the blocking pool and answer its error the way
/// the CLI's `--json` does.
async fn host_blocking<F>(label: &'static str, op: F) -> Response
where
    F: FnOnce() -> Result<Response, OrbitError> + Send + 'static,
{
    match tokio::task::spawn_blocking(op).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => failure(host_error_code(&error), error.to_string(), None),
        Err(join) => failure("internal_error", format!("{label} panicked: {join}"), None),
    }
}
