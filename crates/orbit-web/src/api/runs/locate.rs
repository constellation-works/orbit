//! Find the workspace that holds a run on this host, for the run-detail and
//! run-events reads.
//!
//! A run link names a workspace, but the run's records may sit under a
//! different workspace id in this host's store: the link carried the
//! dashboard's selected workspace, or the run was a pull follower's claimed
//! leaf recorded under a workspace that was later removed from the host. The
//! store keys runs by `(workspace_id, run_id)`, so the records are still
//! readable; only the workspace-scoped runtime cannot see them. This module
//! resolves the requested workspace first and then every workspace id the
//! host store records the run under, and explains a run it cannot find.

use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use orbit_core::application::job::{JobRunHolder, RecordedJobRun};
use orbit_core::{JobRun, NotFoundKind, OrbitError, OrbitRuntime};
use serde_json::json;

use crate::state::{DashboardState, Pinned};

/// Where a run was found.
pub(super) enum LocatedRun {
    /// In a workspace registered and active on this host: its runtime serves
    /// the full detail.
    Registered {
        runtime: Arc<OrbitRuntime>,
        run: Box<JobRun>,
    },
    /// Under a workspace id this host's store holds but no registered
    /// runtime serves. `reader` is a registered runtime used only to reach
    /// the host store and keyed task titles.
    Recorded {
        reader: Arc<OrbitRuntime>,
        recorded: Box<RecordedJobRun>,
    },
}

/// Why a run could not be located; rendered as the response.
pub(super) enum RunLookupFailure {
    Response(Box<Response>),
    Error(Box<OrbitError>),
}

impl RunLookupFailure {
    fn response(response: Response) -> Self {
        Self::Response(Box::new(response))
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

/// Locate `run_id` for a read, on the blocking pool.
///
/// Order: the requested workspace (or the default when none is named) when
/// it is registered and holds the run; then every workspace id the host
/// store records the run under, preferring the requested id; a registered
/// holder is served by its own runtime, an unregistered one from the store.
/// `reconcile` reads the run through the registered runtime's reconciling
/// read; a recorded run is never reconciled, since no live runtime owns it.
pub(super) fn locate_run(
    state: &DashboardState,
    requested: Option<&str>,
    run_id: &str,
    reconcile: impl Fn(&OrbitRuntime, &str) -> Result<JobRun, OrbitError>,
) -> Result<LocatedRun, RunLookupFailure> {
    let pinned = state.pin();
    let selected = requested
        .map(str::to_string)
        .or_else(|| pinned.default_workspace().map(str::to_string));
    let selected_runtime = selected.as_deref().map(|id| pinned.runtime_for(id));
    if let Some(Ok(runtime)) = &selected_runtime {
        match reconcile(runtime, run_id) {
            Ok(run) => {
                return Ok(LocatedRun::Registered {
                    runtime: Arc::clone(runtime),
                    run: Box::new(run),
                });
            }
            Err(OrbitError::NotFound {
                kind: NotFoundKind::JobRun,
                ..
            }) => {}
            Err(error) => return Err(error.into()),
        }
    }
    let Some(reader) = host_store_reader(&pinned, selected_runtime) else {
        return Err(RunLookupFailure::response(not_found(
            run_id,
            format!("run not found: {run_id}"),
            None,
        )));
    };
    let holders = reader.job_run_recording_workspaces(run_id)?;
    let chosen = match holders.as_slice() {
        [] => return Err(RunLookupFailure::response(absent(&reader, run_id)?)),
        [only] => only.clone(),
        many => match requested.filter(|id| many.iter().any(|held| held == id)) {
            Some(id) => id.to_string(),
            None => {
                return Err(RunLookupFailure::response(ambiguous(run_id, many)));
            }
        },
    };
    let registered = pinned
        .entries()
        .iter()
        .any(|entry| entry.id == chosen && entry.active);
    if registered && let Ok(runtime) = pinned.runtime_for(&chosen) {
        let run = reconcile(&runtime, run_id)?;
        return Ok(LocatedRun::Registered {
            runtime,
            run: Box::new(run),
        });
    }
    match reader.recorded_job_run(&chosen, run_id)? {
        Some(recorded) => Ok(LocatedRun::Recorded {
            reader,
            recorded: Box::new(recorded),
        }),
        // Pruned between the two reads.
        None => Err(OrbitError::not_found(NotFoundKind::JobRun, run_id.to_string()).into()),
    }
}

/// A registered runtime to read the host store through: the selected one
/// when it opened, else the first active workspace that opens. Every
/// runtime on this host shares the one host store.
fn host_store_reader(
    pinned: &Pinned,
    selected: Option<Result<Arc<OrbitRuntime>, crate::state::WsRejection>>,
) -> Option<Arc<OrbitRuntime>> {
    if let Some(Ok(runtime)) = selected {
        return Some(runtime);
    }
    pinned
        .entries()
        .iter()
        .filter(|entry| entry.active)
        .find_map(|entry| pinned.runtime_for(&entry.id).ok())
}

/// A run this host's store does not hold under any workspace. When a task
/// names it, say which machine holds it and why it cannot be shown here.
fn absent(reader: &OrbitRuntime, run_id: &str) -> Result<Response, OrbitError> {
    let holder = reader.job_run_holder(run_id).unwrap_or_else(|error| {
        tracing::warn!(run_id, %error, "run holder unreadable; reporting the run as not found");
        None
    });
    let Some(JobRunHolder {
        task_id,
        location,
        local,
    }) = holder
    else {
        return Ok(not_found(run_id, format!("run not found: {run_id}"), None));
    };
    let Some(location) = location else {
        return Ok(not_found(
            run_id,
            format!(
                "run {run_id} is not in this host's store; task {task_id} names it \
                 but recorded no execution machine"
            ),
            None,
        ));
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
    Ok(not_found(run_id, message, Some((code, holder))))
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
