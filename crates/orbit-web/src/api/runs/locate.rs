//! Find the workspace that holds a run on this host, for the run-detail and
//! run-events reads, and for cancel and replay (see [`with_run`]).
//!
//! A run link names a workspace, but the run's records may sit under a
//! different workspace id in this host's store: the link carried the
//! dashboard's selected workspace, or the run was a pull follower's claimed
//! leaf recorded under a workspace that was later removed from the host. The
//! store keys runs by `(workspace_id, run_id)`, so the records are still
//! readable; only the workspace-scoped runtime cannot see them. This module
//! resolves the requested workspace first and then every workspace id the
//! host store records the run under, and explains a run it cannot find.

use orbit_core::app::run::{LookupError, Outcome, RunLookup};

use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use orbit_core::application::job::JobRunHolder;
use orbit_core::{Core, OrbitError};
use serde_json::json;

use crate::api::map_runtime_error;
use crate::state::DashboardState;

pub(super) enum RunLookupFailure {
    Response(Box<Response>),
    Error(Box<OrbitError>),
}

impl RunLookupFailure {
    fn response(response: Response) -> Self {
        Self::Response(Box::new(response))
    }

    /// The HTTP response for this failure.
    pub(super) fn into_response(self) -> Response {
        match self {
            Self::Response(response) => *response,
            Self::Error(error) => map_runtime_error(*error),
        }
    }
}

impl From<OrbitError> for RunLookupFailure {
    fn from(error: OrbitError) -> Self {
        Self::Error(Box::new(error))
    }
}

/// The `workspace` query parameter the request named, if any.
pub(super) fn requested_workspace(query: Option<&str>) -> Option<String> {
    url::form_urlencoded::parse(query.unwrap_or_default().as_bytes())
        .find(|(key, _)| key == "workspace")
        .map(|(_, value)| value.into_owned())
        .filter(|value| !value.is_empty())
}

/// Select one host-store reader and provide the app with the pinned workspace resolver.
pub(super) fn with_run<T>(
    state: &DashboardState,
    requested: Option<&str>,
    id: &str,
    execute: impl FnOnce(&Core, RunLookup<'_>) -> Result<Outcome<Result<T, LookupError>>, OrbitError>,
) -> Result<T, RunLookupFailure> {
    let pinned = state.pin();
    let selected = requested
        .or_else(|| pinned.default_workspace())
        .and_then(|ws| pinned.core_for(ws).ok());
    let is_selected = selected.is_some();
    let reader = selected
        .or_else(|| {
            pinned
                .entries()
                .iter()
                .filter(|entry| entry.active)
                .find_map(|entry| pinned.core_for(&entry.id).ok())
        })
        .ok_or_else(|| {
            RunLookupFailure::response(not_found(id, format!("run not found: {id}"), None))
        })?;
    let open = |ws: &str| {
        pinned
            .entries()
            .iter()
            .find(|entry| entry.active && entry.id == ws)
            .and_then(|entry| pinned.core_for(&entry.id).ok())
    };
    let lookup = RunLookup {
        id,
        requested,
        selected: is_selected,
        open: &open,
    };
    let outcome = execute(&reader, lookup)?;
    outcome.value.map_err(|error| match error {
        LookupError::Flow(error) => error.into(),
        LookupError::Absent(holder) => RunLookupFailure::response(absent(id, holder.map(|holder| *holder))),
        LookupError::Ambiguous(workspaces) => RunLookupFailure::response(ambiguous(id, &workspaces)),
        LookupError::Unregistered(workspace) => {
            let body = json!({
                "error": format!("run {id} is recorded under workspace {workspace}, which is not registered on this host, so it cannot be changed here"),
                "run_id": id,
                "code": "run_workspace_unregistered",
            });
            RunLookupFailure::response((StatusCode::CONFLICT, Json(body)).into_response())
        }
    })
}

/// A run this host's store does not hold under any workspace. When a task
/// names it, say which machine holds it and why it cannot be shown here.
fn absent(run_id: &str, holder: Option<JobRunHolder>) -> Response {
    let Some(JobRunHolder {
        task_id,
        location,
        local,
    }) = holder
    else {
        return not_found(run_id, format!("run not found: {run_id}"), None);
    };
    let Some(location) = location else {
        return not_found(
            run_id,
            format!(
                "run {run_id} is not in this host's store; task {task_id} names it \
                 but recorded no execution machine"
            ),
            None,
        );
    };
    let host = location
        .machine_name
        .clone()
        .unwrap_or_else(|| location.machine_id.clone());
    let holder = json!({
        "task_id": task_id,
        "machine_id": location.machine_id,
        "machine_name": location.machine_name,
        "local": local,
    });
    let (code, message) = if local {
        (
            "run_records_absent",
            format!(
                "run {run_id} ran on this host ({host}) for task {task_id}, \
                 but its records are no longer in this host's store"
            ),
        )
    } else {
        (
            "run_held_elsewhere",
            format!(
                "run {run_id} ran on host {host} for task {task_id}; its records \
                 are held in that host's store, not this one. Open the run on {host}"
            ),
        )
    };
    not_found(run_id, message, Some((code, holder)))
}

/// The run id is recorded under several workspaces and the request named
/// none of them.
fn ambiguous(run_id: &str, workspaces: &[String]) -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({
            "error": format!(
                "run {run_id} is recorded under more than one workspace on this host ({}); \
                 pass ?workspace=<id> to choose one",
                workspaces.join(", ")
            ),
            "code": "run_workspace_ambiguous",
            "workspaces": workspaces,
        })),
    )
        .into_response()
}

/// A 404 for `run_id`, with a stable code and the holding machine when a
/// task records one.
fn not_found(
    run_id: &str,
    message: String,
    holder: Option<(&'static str, serde_json::Value)>,
) -> Response {
    let mut body = json!({ "error": message, "run_id": run_id });
    if let Some((code, holder)) = holder {
        body["code"] = json!(code);
        body["holder"] = holder;
    }
    (StatusCode::NOT_FOUND, Json(body)).into_response()
}
