//! `GET /api/job-runs/all`: a bounded job-run list across every workspace.

use std::collections::{BTreeMap, HashSet};

use axum::extract::State;
use axum::response::{IntoResponse, Json, Response};
use orbit_core::application::job::{JobRunListParams, JobRunOrder};
use orbit_core::{JobRun, JobRunState, OrbitRuntime};
use serde::Deserialize;
use serde_json::{Value, json};

use super::super::jobs::{FAILED_RUN_STATES, JobRunScope, resolve_job_run_scope};
use super::super::{HISTORY_DEFAULT_LIMIT, bad_request, blocking, bounded_limit};
use super::fan_out::fan_out;
use super::list::entry_is_owned;
use crate::state::DashboardState;

#[derive(Deserialize, Default)]
pub(in crate::api) struct AllJobRunsQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    job_id: Option<String>,
    #[serde(default)]
    task_id: Option<String>,
    /// Duration (`24h`) or timestamp. Empty and `all` mean no time bound.
    #[serde(default)]
    since: Option<String>,
}

#[derive(Clone, Copy)]
pub(super) enum AllJobRunsState {
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
/// a genuine zero-run result. `total` sums every available workspace's
/// matching runs, independent of `limit`, so a count (the Jobs rail) can ask
/// for a one-item page.
///
/// Each per-workspace query already asks the store to order and truncate by
/// [`JobRunOrder::Recency`] — the same [`JobRun::recency_at`] this handler later
/// merge-sorts by — so an old, long-running run that only just finished
/// cannot be dropped by a workspace's `limit` before its recency ever gets
/// compared (ORB-11251). An index over that recency expression bounds each
/// workspace's read to its page.
///
/// The listing is observational: it never runs the stale-run reconciliation
/// that operator reads (`/api/job-runs`, `orbit job runs`) and workspace open
/// perform, so a dashboard poll costs reads only.
pub(in crate::api) async fn list_all_job_runs(
    State(state): State<DashboardState>,
    axum::extract::Query(query): axum::extract::Query<AllJobRunsQuery>,
) -> Response {
    let limit = bounded_limit(query.limit, HISTORY_DEFAULT_LIMIT);
    let state_filter = match AllJobRunsState::parse(query.state.as_deref()) {
        Ok(filter) => filter,
        Err(message) => return bad_request(message),
    };
    let scope = match resolve_job_run_scope(
        query.job_id.as_deref(),
        query.task_id.as_deref(),
        query.since.as_deref(),
    ) {
        Ok(scope) => scope,
        Err(message) => return bad_request(message),
    };
    match blocking("aggregate job run list", move || {
        Ok::<_, orbit_core::OrbitError>(all_job_runs_json(&state, limit, state_filter, &scope))
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

pub(super) fn all_job_runs_json(
    state: &DashboardState,
    limit: usize,
    state_filter: AllJobRunsState,
    scope: &JobRunScope,
) -> Value {
    let pinned = state.pin();
    let entries: Vec<&crate::state::WsEntry> = pinned
        .entries()
        .iter()
        .filter(|entry| entry_is_owned(entry))
        .collect();
    // Per workspace, read only its bounded page (at most `limit` runs), its
    // exact count and its page's task titles. Each workspace's reads are
    // independent (its own runtime, run store and task store, whose title
    // read takes that partition's task-commit lock), so they run
    // concurrently and the request waits for the slowest workspace instead
    // of the sum of twelve lock-and-read round trips; the fold below runs in
    // pinned entry order, so totals and `unavailable` keep their serial
    // order. The expensive
    // projection — reading each candidate's pipeline state and lineage —
    // is deferred until after the global merge below: only runs that
    // survive `truncate(limit)` are projected, so a one-item page pays
    // that cost for one run rather than for every workspace's candidate.
    // Phase tracing on a 13-workspace fixture showed that discarded
    // candidates' state reads, not the index-covered page or count,
    // dominated the failed-runs tail.
    let selections = fan_out(entries.len(), |index| {
        let entry = &entries[index];
        if !entry.active {
            return Err("workspace is unavailable".to_string());
        }
        let runtime = match pinned.runtime_for(&entry.id) {
            Ok(runtime) => runtime,
            Err(_) => return Err("failed to open workspace".to_string()),
        };
        let page = workspace_job_runs(&runtime, limit, state_filter, scope).and_then(|runs| {
            Ok((
                runs,
                workspace_job_run_count(&runtime, state_filter, scope)?,
            ))
        });
        match page {
            Ok((runs, count)) => match super::super::runs::task_titles(&runtime, &runs) {
                Ok(titles) => Ok((runtime, runs, count, titles)),
                Err(error) => Err(error.to_string()),
            },
            Err(error) => Err(error.to_string()),
        }
    });

    let mut candidates: Vec<(JobRun, usize)> = Vec::new();
    let mut projections: BTreeMap<
        usize,
        (
            std::sync::Arc<OrbitRuntime>,
            std::collections::BTreeMap<String, String>,
        ),
    > = BTreeMap::new();
    let mut unavailable = Vec::new();
    let mut total = 0_u64;
    for (index, (entry, selection)) in entries.iter().zip(selections).enumerate() {
        match selection {
            Ok((runtime, runs, count, titles)) => {
                total = total.saturating_add(count);
                projections.insert(index, (runtime, titles));
                candidates.extend(runs.into_iter().map(|run| (run, index)));
            }
            Err(error) => unavailable.push(json!({
                "workspace_id": entry.id,
                "workspace_name": entry.name,
                "error": error,
            })),
        }
    }

    candidates.sort_by(|(left, left_index), (right, right_index)| {
        right
            .recency_at()
            .cmp(&left.recency_at())
            .then_with(|| entries[*left_index].id.cmp(&entries[*right_index].id))
            .then_with(|| left.run_id.cmp(&right.run_id))
    });
    candidates.truncate(limit);

    // Project only the surviving page, grouped by owning workspace so each
    // workspace's pipeline states are read once for its returned runs.
    let mut values: Vec<Option<Value>> = candidates.iter().map(|_| None).collect();
    let mut positions_by_workspace: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (position, (_, index)) in candidates.iter().enumerate() {
        positions_by_workspace
            .entry(*index)
            .or_default()
            .push(position);
    }
    for (index, positions) in &positions_by_workspace {
        let Some((runtime, titles)) = projections.get(index) else {
            continue;
        };
        let runs = positions
            .iter()
            .map(|position| candidates[*position].0.clone())
            .collect::<Vec<_>>();
        let rows = super::super::runs::run_list_rows(runtime, &runs, titles);
        for (position, value) in positions.iter().zip(rows) {
            values[*position] = Some(value);
        }
    }
    let mut candidates = candidates
        .into_iter()
        .zip(values)
        .map(|((run, index), value)| {
            (
                run,
                entries[index].id.clone(),
                entries[index].name.clone(),
                // `run_list_rows` returns one row per run; the fallback only
                // covers a projection map miss, which cannot happen for a
                // workspace that produced candidates.
                value.unwrap_or_else(|| json!({})),
            )
        })
        .collect::<Vec<_>>();

    let present: HashSet<(String, String)> = candidates
        .iter()
        .map(|(run, workspace_id, _, _)| (workspace_id.clone(), run.run_id.clone()))
        .collect();
    for (_, workspace_id, _, value) in &mut candidates {
        if let Some(parent) = value.get("parent_run_id").and_then(Value::as_str)
            && !present.contains(&(workspace_id.clone(), parent.to_string()))
        {
            value["parent_run_id"] = Value::Null;
        }
    }
    let truncated = total > candidates.len() as u64;
    let items = candidates
        .into_iter()
        .map(|(_, workspace_id, workspace_name, mut value)| {
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
        "total": total,
        "truncated": truncated,
        "unavailable": unavailable,
    })
}

fn workspace_job_runs(
    runtime: &OrbitRuntime,
    limit: usize,
    state_filter: AllJobRunsState,
    scope: &JobRunScope,
) -> Result<Vec<JobRun>, orbit_core::OrbitError> {
    runtime.list_job_runs_observed(workspace_job_run_params(scope, state_filter, Some(limit)))
}

/// How many runs in one workspace match the filter, independent of `limit`.
fn workspace_job_run_count(
    runtime: &OrbitRuntime,
    state_filter: AllJobRunsState,
    scope: &JobRunScope,
) -> Result<u64, orbit_core::OrbitError> {
    runtime.count_job_runs_observed(workspace_job_run_params(scope, state_filter, None))
}

fn workspace_job_run_params(
    scope: &JobRunScope,
    state_filter: AllJobRunsState,
    limit: Option<usize>,
) -> JobRunListParams {
    let states = match state_filter {
        AllJobRunsState::All => Vec::new(),
        AllJobRunsState::Failed => FAILED_RUN_STATES.to_vec(),
        AllJobRunsState::Active => vec![JobRunState::Pending, JobRunState::Running],
    };
    JobRunListParams {
        job_id: scope.job_id.clone(),
        task_id: scope.task_id.clone(),
        states,
        since: scope.since,
        limit,
        order_by: JobRunOrder::Recency,
        ..Default::default()
    }
}
