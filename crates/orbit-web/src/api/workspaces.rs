//! Global (cross-workspace) endpoints (ORB-00030).
//!
//! These handlers take the whole [`DashboardState`] rather than a single
//! workspace runtime (via the [`Ws`](crate::state::Ws) extractor), because they
//! describe or aggregate across every servable workspace. In single mode they
//! degrade gracefully: `/api/workspaces` reports the one synthetic entry and
//! `/api/tasks/all` returns that workspace's tasks.

use std::collections::BTreeMap;
use std::path::Path;

use axum::extract::{RawQuery, State};
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Utc};
use orbit_core::application::job::{JobRunListParams, JobRunOrder, job_run_to_json};
use orbit_core::{JobRun, JobRunState, OrbitRuntime};
use serde::Deserialize;
use serde_json::{Value, json};

use super::pagination::TaskPageQuery;
use super::{HISTORY_DEFAULT_LIMIT, bad_request, blocking, bounded_limit, server_error};
use crate::projections::TaskListProjection;
use crate::state::{DashboardState, Pinned};

/// `GET /api/workspaces` — list every workspace the dashboard can serve, with
/// the currently-selected default flagged.
///
/// Filesystem paths (`root`, `orbit_dir`) are home-abbreviated to `~` so the
/// frontend can render the selected workspace's location directly, without
/// needing to know the server's home directory (ORB-00037).
pub(super) async fn list_workspaces(State(state): State<DashboardState>) -> Response {
    match blocking("list workspaces", move || Ok(list_workspaces_json(&state))).await {
        Ok(values) => Json(Value::Array(values)).into_response(),
        Err(response) => *response,
    }
}

fn list_workspaces_json(state: &DashboardState) -> Vec<Value> {
    // Refresh and pin one snapshot so the listing and its `is_default` flags all
    // reflect the same generation (add/remove/rebind observed atomically).
    let pinned = state.pin();
    let default = pinned.default_workspace();
    let home = orbit_common::fs::path::home_dir().ok();
    pinned
        .entries()
        .iter()
        .map(|entry| {
            json!({
                "id": entry.id,
                "name": entry.name,
                "root": abbreviate_home(&entry.repo_root, home.as_deref()),
                "orbit_dir": abbreviate_home(&entry.orbit_dir, home.as_deref()),
                "status": if entry.active { "active" } else { "invalid" },
                "is_default": default == Some(entry.id.as_str()),
            })
        })
        .collect()
}

/// `GET /api/tasks/all` — dashboard tasks aggregated across active workspaces.
///
/// Each task object is tagged with its owning workspace (`workspace_id` /
/// `workspace_name`, plus the home-abbreviated `workspace_root` path added in
/// ORB-00037) so the frontend can badge the row and show the full location in
/// the task's Details box. Inactive (stale-path) workspaces are skipped, as are
/// any that fail to open — the aggregate view stays available even when one
/// workspace is broken. Status/tag/type/search filters, limit, and cursor have
/// the same semantics as `/api/tasks`; selection happens independently in each
/// workspace before one global `created_at DESC, id ASC` merge.
pub(super) async fn list_all_tasks(
    State(state): State<DashboardState>,
    RawQuery(raw_query): RawQuery,
) -> Response {
    let query = match TaskPageQuery::parse(raw_query.as_deref()) {
        Ok(query) => query,
        Err(message) => return bad_request(message),
    };
    match blocking("aggregate task list", move || {
        let pinned = state.pin();
        let scope = aggregate_task_scope(&pinned);
        let mut query = query;
        query
            .bind_cursor(&scope)
            .map_err(orbit_core::OrbitError::InvalidInput)?;
        Ok(all_tasks_json(&pinned, &query, &scope))
    })
    .await
    {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => server_error(error),
        Err(response) => *response,
    }
}

fn aggregate_task_scope(pinned: &Pinned) -> String {
    let mut workspace_ids = pinned
        .entries()
        .iter()
        .filter(|entry| entry.active)
        .map(|entry| entry.id.as_str())
        .collect::<Vec<_>>();
    workspace_ids.sort_unstable();
    format!("aggregate:{}", workspace_ids.join(","))
}

#[derive(Deserialize, Default)]
pub(super) struct AllJobRunsQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    state: Option<String>,
}

#[derive(Clone, Copy)]
enum AllJobRunsState {
    All,
    Active,
    Failed,
}

impl AllJobRunsState {
    fn parse(value: Option<&str>) -> Result<Self, String> {
        match value {
            None | Some("all") => Ok(Self::All),
            Some("active") => Ok(Self::Active),
            Some("failed") => Ok(Self::Failed),
            Some(_) => Err("invalid state; expected one of: all, active, failed".to_string()),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Active => "active",
            Self::Failed => "failed",
        }
    }
}

/// `GET /api/job-runs/all` — a bounded run list across every visible workspace.
///
/// Run ids are workspace-local, so every item carries its workspace identity.
/// Unavailable sources remain in the response instead of being represented as
/// an empty workspace, allowing the dashboard to distinguish partial data from
/// a genuine zero-run result.
///
/// Each per-workspace query already asks the store to order and truncate by
/// [`JobRunOrder::Recency`] — the same `run_timestamp` this handler later
/// merge-sorts by — so an old, long-running run that only just finished
/// cannot be dropped by a workspace's `limit` before its recency ever gets
/// compared (ORB-11251).
pub(super) async fn list_all_job_runs(
    State(state): State<DashboardState>,
    axum::extract::Query(query): axum::extract::Query<AllJobRunsQuery>,
) -> Response {
    let limit = bounded_limit(query.limit, HISTORY_DEFAULT_LIMIT);
    let state_filter = match AllJobRunsState::parse(query.state.as_deref()) {
        Ok(filter) => filter,
        Err(message) => return bad_request(message),
    };
    match blocking("aggregate job run list", move || {
        Ok::<_, orbit_core::OrbitError>(all_job_runs_json(&state, limit, state_filter))
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

fn all_job_runs_json(state: &DashboardState, limit: usize, state_filter: AllJobRunsState) -> Value {
    let pinned = state.pin();
    let mut candidates = Vec::new();
    let mut unavailable = Vec::new();
    let mut source_truncated = false;

    for entry in pinned.entries() {
        if !entry.active {
            unavailable.push(json!({
                "workspace_id": entry.id,
                "workspace_name": entry.name,
                "error": "workspace is unavailable",
            }));
            continue;
        }
        let runtime = match pinned.runtime_for(&entry.id) {
            Ok(runtime) => runtime,
            Err(_) => {
                unavailable.push(json!({
                    "workspace_id": entry.id,
                    "workspace_name": entry.name,
                    "error": "failed to open workspace",
                }));
                continue;
            }
        };
        match workspace_job_runs(&runtime, limit, state_filter) {
            Ok(runs) => {
                source_truncated |= runs.len() == limit;
                candidates.extend(
                    runs.into_iter()
                        .map(|run| (run, entry.id.clone(), entry.name.clone())),
                );
            }
            Err(error) => unavailable.push(json!({
                "workspace_id": entry.id,
                "workspace_name": entry.name,
                "error": error.to_string(),
            })),
        }
    }

    candidates.sort_by(|(left, left_workspace, _), (right, right_workspace, _)| {
        run_timestamp(right)
            .cmp(&run_timestamp(left))
            .then_with(|| left_workspace.cmp(right_workspace))
            .then_with(|| left.run_id.cmp(&right.run_id))
    });
    let truncated = source_truncated || candidates.len() > limit;
    candidates.truncate(limit);
    let items = candidates
        .into_iter()
        .map(|(run, workspace_id, workspace_name)| {
            let mut value = job_run_to_json(&run, None);
            if let Value::Object(map) = &mut value {
                map.insert("workspace_id".to_string(), json!(workspace_id));
                map.insert("workspace_name".to_string(), json!(workspace_name));
            }
            value
        })
        .collect::<Vec<_>>();

    json!({
        "items": items,
        "limit": limit,
        "state": state_filter.label(),
        "truncated": truncated,
        "unavailable": unavailable,
    })
}

fn workspace_job_runs(
    runtime: &OrbitRuntime,
    limit: usize,
    state_filter: AllJobRunsState,
) -> Result<Vec<JobRun>, orbit_core::OrbitError> {
    let list = |state| {
        runtime.list_job_runs(JobRunListParams {
            state,
            limit: Some(limit),
            order_by: JobRunOrder::Recency,
            ..Default::default()
        })
    };
    match state_filter {
        AllJobRunsState::All => list(None),
        AllJobRunsState::Failed => list(Some(JobRunState::Failed)),
        AllJobRunsState::Active => {
            let mut runs = list(Some(JobRunState::Pending))?;
            runs.extend(list(Some(JobRunState::Running))?);
            Ok(runs)
        }
    }
}

fn run_timestamp(run: &JobRun) -> DateTime<Utc> {
    run.finished_at.or(run.started_at).unwrap_or(run.created_at)
}

fn all_tasks_json(
    pinned: &Pinned,
    query: &TaskPageQuery,
    scope: &str,
) -> Result<Value, orbit_core::OrbitError> {
    let home = orbit_common::fs::path::home_dir().ok();
    let mut candidates = Vec::new();
    let mut total = 0;
    let mut remaining = 0;
    for entry in pinned.entries().iter().filter(|entry| entry.active) {
        let Ok(runtime) = pinned.runtime_for(&entry.id) else {
            continue;
        };
        // One broken workspace must not take down the fleet-wide list.
        let page = match runtime.task_candidates(&query.filter(), query.limit()) {
            Ok(page) => page,
            Err(error) => {
                tracing::warn!(workspace = %entry.id, %error, "fleet task list skipped a workspace");
                continue;
            }
        };
        total += page.total_without_cursor;
        remaining += page.total;
        for task in page.items {
            candidates.push((task, runtime.clone(), entry));
        }
    }
    candidates.sort_by(|(a, _, _), (b, _, _)| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    candidates.truncate(query.limit());
    let offset = query.offset();
    let next_offset = offset.saturating_add(candidates.len());
    let next_cursor = if remaining > candidates.len() {
        candidates
            .last()
            .map(|(task, _, _)| {
                query.next_cursor(scope, task.created_at, task.id.clone(), next_offset)
            })
            .transpose()
            .map_err(|error| {
                orbit_core::OrbitError::Execution(format!("encode task cursor: {error}"))
            })?
    } else {
        None
    };
    // All runtimes in a dashboard share one coordination registry. Read its
    // global dependency projection once, after the metadata selection.
    let statuses = candidates
        .first()
        .map(|(_, runtime, _)| runtime.task_status_index())
        .transpose()?
        .unwrap_or_default();
    // Each workspace's runtime carries its own crew registry; build each one
    // once for the page rather than once per row.
    let mut projections: BTreeMap<&str, TaskListProjection> = BTreeMap::new();
    let mut values = Vec::with_capacity(candidates.len());
    for (task, runtime, entry) in candidates {
        let Some(row) = runtime.get_listed_task_row(&task.id)? else {
            continue;
        };
        let projection = projections
            .entry(entry.id.as_str())
            .or_insert_with(|| TaskListProjection::new(&runtime));
        let mut value = projection.row_to_json(&row, &statuses)?;
        if let Value::Object(map) = &mut value {
            map.insert("workspace_id".to_string(), json!(entry.id));
            map.insert("workspace_name".to_string(), json!(entry.name));
            map.insert(
                "workspace_root".to_string(),
                json!(abbreviate_home(&entry.repo_root, home.as_deref())),
            );
        }
        values.push(value);
    }
    Ok(json!({
        "items": values,
        "total": total,
        "limit": query.limit(),
        "truncated": total > values.len(),
        "offset": offset,
        "next_cursor": next_cursor,
    }))
}

/// Render a filesystem path for display, collapsing the user's home directory
/// to `~` (e.g. `/home/dan/ws/orbit` → `~/ws/orbit`). Paths outside `home`, an
/// unset `home`, or an empty path (the single-mode synthetic entry) are rendered
/// verbatim. ORB-00037.
pub(super) fn abbreviate_home(path: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home {
        if path == home {
            return "~".to_string();
        }
        if let Ok(rest) = path.strip_prefix(home) {
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}
