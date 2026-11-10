//! Global (cross-workspace) endpoints (ORB-00030).
//!
//! These handlers take the whole [`DashboardState`] rather than a single
//! workspace runtime (via the [`Ws`](crate::state::Ws) extractor), because they
//! describe or aggregate across every servable workspace. In single mode they
//! degrade gracefully: `/api/workspaces` reports the one synthetic entry and
//! `/api/tasks/all` returns that workspace's tasks.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::extract::{RawQuery, State};
use axum::response::{IntoResponse, Json, Response};
use orbit_core::application::job::{JobRunListParams, JobRunOrder};
use orbit_core::application::task::{TaskCandidateKey, TaskCandidateKeys};
use orbit_core::{FleetReadinessFacts, JobRun, JobRunState, OrbitRuntime};
use orbit_types::workspace::WorkspaceCheckoutRole;
use serde::Deserialize;
use serde_json::{Value, json};

use super::jobs::{FAILED_RUN_STATES, JobRunScope, resolve_job_run_scope};
use super::pagination::TaskPageQuery;
use super::{HISTORY_DEFAULT_LIMIT, bad_request, blocking, bounded_limit, server_error};
use crate::projections::{TaskListProjection, cached_riskiness_model};
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

/// Whether this host owns the checkout for `entry`. A binding with no
/// recorded role (single-host installs, older registrations) counts as owned.
pub(crate) fn entry_is_owned(entry: &crate::state::WsEntry) -> bool {
    entry
        .binding
        .as_ref()
        .and_then(|binding| binding.checkout_role)
        != Some(WorkspaceCheckoutRole::Replica)
}

fn list_workspaces_json(state: &DashboardState) -> Vec<Value> {
    // Refresh and pin one snapshot so the listing and its `is_default` flags all
    // reflect the same generation (add/remove/rebind observed atomically).
    let pinned = state.pin();
    let default = pinned.default_workspace();
    let home = orbit_common::fs::path::home_dir().ok();
    let entries = pinned.entries();
    // `lease_enabled` lets the dashboard hide its Leases destination where
    // leases are off; null when the workspace's runtime can't be opened, so an
    // unknown setting reads as today's behaviour (shown).
    let lease_enabled = fan_out(entries.len(), |index| {
        let entry = &entries[index];
        if !entry.active {
            return None;
        }
        let runtime = pinned.runtime_for(&entry.id).ok()?;
        Some(runtime.leases_enabled())
    });
    entries
        .iter()
        .zip(lease_enabled)
        .map(|(entry, lease_enabled)| {
            json!({
                "id": entry.id,
                "creation_binding": super::tasks::create::binding_token(entry),
                "name": entry.name,
                "root": abbreviate_home(&entry.repo_root, home.as_deref()),
                "orbit_dir": abbreviate_home(&entry.orbit_dir, home.as_deref()),
                "status": if entry.active { "active" } else { "invalid" },
                "is_default": default == Some(entry.id.as_str()),
                "lease_enabled": lease_enabled,
                "checkout_role": entry.binding.as_ref().and_then(|binding| binding.checkout_role).map(|role| role.as_str()),
                "owner_machine_id": entry.binding.as_ref().and_then(|binding| binding.owner_machine_id.clone()),
            })
        })
        .collect()
}

/// Upper bound on the scoped threads one aggregate request fans out to.
/// Each workspace read is independent (its own runtime and store), so the
/// aggregate waits for the slowest workspace instead of the sum; the bound
/// keeps a host with many registered workspaces from spawning a thread per
/// workspace per poll.
const MAX_AGGREGATE_FAN_OUT: usize = 8;

/// Run `work(index)` for every index in `0..count` on up to
/// [`MAX_AGGREGATE_FAN_OUT`] scoped threads, returning the results in
/// index order. Each thread takes the next unclaimed index, so one slow
/// workspace delays no other. A panicking worker re-raises in the caller,
/// exactly as the serial loop would.
fn fan_out<T>(count: usize, work: impl Fn(usize) -> T + Sync) -> Vec<T>
where
    T: Send,
{
    if count <= 1 {
        return (0..count).map(work).collect();
    }
    let workers = std::thread::available_parallelism()
        .map(|parallelism| parallelism.get())
        .unwrap_or(4)
        .min(MAX_AGGREGATE_FAN_OUT)
        .min(count);
    let next_index = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let handles = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut claimed = Vec::new();
                    loop {
                        let index = next_index.fetch_add(1, Ordering::Relaxed);
                        if index >= count {
                            return claimed;
                        }
                        claimed.push((index, work(index)));
                    }
                })
            })
            .collect::<Vec<_>>();
        let mut results = Vec::with_capacity(count);
        for handle in handles {
            match handle.join() {
                Ok(claimed) => results.extend(claimed),
                Err(payload) => std::panic::resume_unwind(payload),
            }
        }
        results.sort_unstable_by_key(|(index, _)| *index);
        results.into_iter().map(|(_, result)| result).collect()
    })
}

/// `GET /api/fleet/drains` — one observational readiness row per registered
/// workspace on this host. Unavailable workspaces remain explicit rows. Remote
/// hosts expose the same endpoint through the existing single-hop forward.
pub(super) async fn list_fleet_drains(State(state): State<DashboardState>) -> Response {
    match blocking("fleet drains", move || {
        let pinned = state.pin();
        let entries = pinned.entries();
        // Every workspace here shares one task registry, so the first runtime
        // that opens reads its global facts for all of them.
        let facts = entries
            .iter()
            .filter(|entry| entry.active)
            .find_map(|entry| pinned.runtime_for(&entry.id).ok())
            .map(|runtime| runtime.fleet_readiness_facts())
            .unwrap_or_default();
        // One readiness snapshot per workspace, computed concurrently: the
        // row order stays the pinned entries' order.
        let items = fan_out(entries.len(), |index| {
            fleet_drain_row(&pinned, &entries[index], &facts)
        });
        Ok::<_, orbit_core::OrbitError>(json!({"items": items}))
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

fn fleet_drain_row(
    pinned: &Pinned,
    entry: &crate::state::WsEntry,
    facts: &FleetReadinessFacts,
) -> Value {
    let mut row = json!({
        "workspace_id": entry.id,
        "workspace_name": entry.name,
        "phase": "unavailable",
        "slots_used": null,
        "slots_ceiling": null,
        "eligible_count": null,
        "top_wait_reason": null,
        "holds": [],
        "queued_drains": [],
        "task_pilot_warning": null,
        "drain_phase": null,
        "pull_active": null,
        "pull_admissions_stopped": null,
    });
    let readiness = if entry.active {
        pinned
            .runtime_for(&entry.id)
            .map_err(|error| orbit_core::OrbitError::Execution(format!("{error:?}")))
            .and_then(|runtime| {
                let mut payload = runtime.workspace_fleet_readiness(facts)?;
                payload["owner_machine_id"] = json!(runtime.replica_owner_machine());
                Ok(payload)
            })
    } else {
        row["error"] = json!("workspace is unavailable");
        return row;
    };
    match readiness {
        Ok(payload) => {
            let tasks = payload["tasks"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let eligible = tasks.iter().filter(|task| task["eligible"] == true).count();
            let mut waits = BTreeMap::<&str, usize>::new();
            for task in tasks.iter().filter(|task| task["eligible"] != true) {
                if let Some(reason) = task["reason"].as_str() {
                    *waits.entry(reason).or_default() += 1;
                }
            }
            // Stable alphabetical tie-break; never return task-specific
            // detail (which can contain internal task identifiers).
            let top = waits
                .iter()
                .max_by(|(a, ac), (b, bc)| ac.cmp(bc).then_with(|| b.cmp(a)))
                .map(|(reason, _)| *reason);
            let capacity = &payload["capacity"];
            let mut holds = Vec::new();
            if capacity["admissions_stopped"] == true
                || capacity["pull_drain_admissions_stopped"] == true
            {
                holds.push(json!("admissions stopped"));
            }
            if capacity["host_shutdown_holding"] == true {
                holds.push(json!("host shutdown scheduled"));
            }
            if let Some(resources) = capacity["resource_throttle"]["resources"].as_array() {
                for resource in resources {
                    if let Some(name) = resource["resource"].as_str() {
                        holds.push(json!(format!("{name} throttled")));
                    }
                }
            }
            // A crew that sets `env` is its own account, `provider/crew`.
            let account = |entry: &Value| {
                let provider = entry["provider"].as_str()?;
                Some(orbit_types::workflow::provider_account_label(
                    provider,
                    entry["account"].as_str(),
                ))
            };
            for limit in payload["provider_limits"].as_array().into_iter().flatten() {
                if limit["gated"] == true
                    && let Some(account) = account(limit)
                {
                    holds.push(json!(format!("{account} usage limit")));
                }
            }
            for failure in payload["provider_auth_failures"]
                .as_array()
                .into_iter()
                .flatten()
            {
                if let Some(account) = account(failure) {
                    holds.push(json!(format!("{account} authentication failed")));
                }
            }
            let phase =
                if capacity["drain_phase"] == "idle" && capacity["pull_drain_run_id"].is_string() {
                    "pulling"
                } else {
                    capacity["drain_phase"].as_str().unwrap_or("idle")
                };
            row["phase"] = json!(phase);
            // Keep the independent drain and pull states for Settings Hosts;
            // the fleet's combined `phase` alone loses a concurrent pull.
            row["drain_phase"] = json!(capacity["drain_phase"].as_str().unwrap_or(
                if capacity["drain_run_id"].is_string() {
                    "draining"
                } else {
                    "idle"
                }
            ));
            row["pull_active"] = json!(capacity["pull_drain_run_id"].is_string());
            row["pull_admissions_stopped"] =
                json!(capacity["pull_drain_admissions_stopped"] == true);
            row["owner_machine_id"] = payload["owner_machine_id"].clone();
            // Pull coordinators execute the local workspace's claimed leaves,
            // rather than admitting owner workers into their own checkpoint.
            row["running_tasks"] = if phase == "pulling" {
                capacity["wrapper_leaf_runs"].clone()
            } else {
                capacity["running_admitted_workers"].clone()
            };
            row["slots_used"] = capacity["active_leaf_runs"].clone();
            row["slots_ceiling"] = capacity["max_active_leaf_runs"].clone();
            row["eligible_count"] = json!(eligible);
            row["counts_truncated"] =
                json!(payload["total"].as_u64().unwrap_or(0) > tasks.len() as u64);
            row["top_wait_reason"] = json!(top);
            row["holds"] = json!(holds);
            // Windows waiting for the live one's slot, as readiness reports
            // them: run ids, policy and crew names, never env values.
            row["queued_drains"] = capacity["queued_drains"].clone();
            // The live drain's own record that no task pilot runs here.
            row["task_pilot_warning"] = capacity["task_pilot_warning"].clone();
        }
        Err(error) => {
            tracing::warn!(workspace = %entry.id, %error, "fleet readiness unavailable");
            row["error"] = json!("readiness unavailable");
        }
    }
    row
}

/// `GET /api/tasks/all` — dashboard tasks aggregated across active workspaces.
///
/// Each task object is tagged with its owning workspace (`workspace_id` /
/// `workspace_name`, plus the home-abbreviated `workspace_root` path added in
/// ORB-00037) so the frontend can badge the row and show the full location in
/// the task's Details box. Inactive workspaces, workspaces that fail to open,
/// and workspaces whose task read fails stay out of `items` and are named in
/// `unavailable` (`workspace_id`, `workspace_name`, `error`), the same object
/// `/api/job-runs/all` uses. One unreadable task row marks that workspace
/// unavailable for the response while rows that did read stay in `items`. The
/// aggregate stays available when one workspace is broken. `unavailable` is an
/// empty array when every source answered. Status/tag/type/search filters,
/// limit, and cursor have the same semantics as `/api/tasks`. Rows are ordered
/// workspace by workspace, in the order `/api/workspaces` lists them (the
/// dashboard's workspace picker order), and `created_at DESC, id ASC` inside
/// one workspace. The page is cut in that order, so a status section that
/// spans pages never shows one workspace's rows on both sides of another's;
/// the cursor names the workspace its last row belongs to. Repeated
/// `workspaces` parameters narrow sources before selection and bind the cursor
/// to that subset. `total` still sums only selected workspaces that answered.
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
        aggregate_resume_index(&aggregate_task_entries(&pinned, &query), &query)?;
        Ok(all_tasks_json(&state, &pinned, &query, &scope))
    })
    .await
    {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => server_error(error),
        Err(response) => *response,
    }
}

/// The workspaces `/api/tasks/all` reads, in `/api/workspaces` order.
fn aggregate_task_entries<'a>(
    pinned: &'a Pinned,
    query: &TaskPageQuery,
) -> Vec<&'a crate::state::WsEntry> {
    pinned
        .entries()
        .iter()
        .filter(|entry| entry_is_owned(entry))
        .filter(|entry| query.workspaces.is_empty() || query.workspaces.contains(&entry.id))
        .collect()
}

/// Where an aggregate page resumes: the index of the cursor's workspace in
/// `entries`. Workspaces before it are spent: they still answer for `total`
/// but contribute no rows. The cursor's own workspace resumes after its last
/// row, and later workspaces start from their newest task.
fn aggregate_resume_index(
    entries: &[&crate::state::WsEntry],
    query: &TaskPageQuery,
) -> Result<usize, orbit_core::OrbitError> {
    let Some(cursor) = query.cursor() else {
        return Ok(0);
    };
    let workspace_id = cursor.workspace_id.as_deref().ok_or_else(|| {
        orbit_core::OrbitError::InvalidInput(
            "task cursor predates workspace-ordered pages; reload the list".to_string(),
        )
    })?;
    entries
        .iter()
        .position(|entry| entry.id == workspace_id)
        .ok_or_else(|| {
            orbit_core::OrbitError::InvalidInput(
                "task cursor names a workspace outside this listing".to_string(),
            )
        })
}

fn aggregate_task_scope(pinned: &Pinned) -> String {
    let mut workspace_ids = pinned
        .entries()
        .iter()
        .filter(|entry| entry.active && entry_is_owned(entry))
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
pub(super) async fn list_all_job_runs(
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
            Ok((runs, count)) => match super::runs::task_titles(&runtime, &runs) {
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
        let rows = super::runs::run_list_rows(runtime, &runs, titles);
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

fn all_tasks_json(
    state: &DashboardState,
    pinned: &Pinned,
    query: &TaskPageQuery,
    scope: &str,
) -> Result<Value, orbit_core::OrbitError> {
    let home = orbit_common::fs::path::home_dir().ok();
    let mut candidates = Vec::new();
    let mut total = 0;
    let mut remaining = 0;
    let mut unavailable = Vec::new();
    let mut unavailable_ids = HashSet::new();
    let entries = aggregate_task_entries(pinned, query);
    let resume_index = aggregate_resume_index(&entries, query)?;
    let resume_before = query
        .cursor()
        .map(|cursor| (cursor.created_at, cursor.id.clone()));
    // Each workspace's candidate keys are selected concurrently (its own
    // runtime and store); the fold below runs in pinned entry order, so
    // totals, candidates and `unavailable` keep their serial order.
    let selections = fan_out(entries.len(), |index| {
        let entry = &entries[index];
        if !entry.active {
            return Err("workspace is unavailable".to_string());
        }
        let runtime = match pinned.runtime_for(&entry.id) {
            Ok(runtime) => runtime,
            Err(_) => {
                tracing::warn!(workspace = %entry.id, "fleet task list could not open a workspace");
                return Err("failed to open workspace".to_string());
            }
        };
        let (filter, limit) = match index.cmp(&resume_index) {
            // Only the total is read from a spent workspace.
            std::cmp::Ordering::Less => (query.filter_scanning_before(None), 1),
            std::cmp::Ordering::Equal => (
                query.filter_scanning_before(resume_before.clone()),
                query.limit(),
            ),
            std::cmp::Ordering::Greater => (query.filter_scanning_before(None), query.limit()),
        };
        // One broken workspace must not take down the fleet-wide list.
        match workspace_task_keys(&runtime, query, filter, limit) {
            Ok(page) => Ok((runtime, page)),
            Err(error) => {
                tracing::warn!(workspace = %entry.id, %error, "fleet task list skipped a workspace");
                Err(error.to_string())
            }
        }
    });
    for (index, (entry, selection)) in entries.iter().zip(selections).enumerate() {
        let (runtime, page) = match selection {
            Ok(selection) => selection,
            Err(error) => {
                record_unavailable(&mut unavailable, &mut unavailable_ids, entry, &error);
                continue;
            }
        };
        total += page.total_without_cursor;
        if index < resume_index {
            continue;
        }
        remaining += page.total;
        let mut items = page.items;
        items.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        for task in items {
            candidates.push((task, runtime.clone(), *entry));
        }
    }
    candidates.truncate(query.limit());
    let offset = query.offset();
    let next_offset = offset.saturating_add(candidates.len());
    let next_cursor = if remaining > candidates.len() {
        candidates
            .last()
            .map(|(task, _, entry)| {
                query.next_workspace_cursor(
                    scope,
                    &entry.id,
                    task.created_at,
                    task.id.clone(),
                    next_offset,
                )
            })
            .transpose()
            .map_err(|error| {
                orbit_core::OrbitError::Execution(format!("encode task cursor: {error}"))
            })?
    } else {
        None
    };
    let mut rows = Vec::with_capacity(candidates.len());
    for (task, runtime, entry) in candidates {
        // Same contract as the per-workspace scan above: one unreadable row
        // is logged and skipped rather than failing the whole fleet list.
        match runtime.get_listed_task_row(&task.id) {
            Ok(Some(row)) => rows.push((row, runtime, entry)),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    workspace = %entry.id,
                    task = %task.id,
                    %error,
                    "fleet task list skipped an unreadable task row"
                );
                // The dropped row would look like a new review task when it
                // returns, unless this read says the workspace was incomplete.
                record_unavailable(
                    &mut unavailable,
                    &mut unavailable_ids,
                    entry,
                    &error.to_string(),
                );
            }
        }
    }
    // All runtimes in a dashboard share one coordination registry. Read its
    // global dependency projection once, after hydrating the page.
    let statuses = rows
        .first()
        .map(|(_, runtime, _)| {
            runtime.dependency_status_index(rows.iter().map(|(row, _, _)| &row.task))
        })
        .transpose()?
        .unwrap_or_default();
    // Each workspace's runtime carries its own crew registry; build each one
    // once for the page rather than once per row.
    // One batched delivery-run read per workspace for the page's rows.
    let mut attention_by_workspace: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
    for (row, runtime, entry) in &rows {
        let _ = row;
        if !attention_by_workspace.contains_key(&entry.id) {
            let workspace_rows = rows
                .iter()
                .filter(|(_, _, other)| other.id == entry.id)
                .map(|(row, _, _)| row);
            let map = runtime.delivery_attention_map(workspace_rows)?;
            attention_by_workspace.insert(entry.id.clone(), map);
        }
    }
    let mut projections: BTreeMap<&str, TaskListProjection> = BTreeMap::new();
    let mut values = Vec::with_capacity(rows.len());
    for (row, runtime, entry) in rows {
        let projection = projections.entry(entry.id.as_str()).or_insert_with(|| {
            TaskListProjection::new(&runtime)
                .with_delivery_attention(
                    attention_by_workspace
                        .get(&entry.id)
                        .cloned()
                        .unwrap_or_default(),
                )
                .with_riskiness(
                    // On the blocking pool: wait for this workspace's model
                    // as the per-workspace list does.
                    tokio::runtime::Handle::current()
                        .block_on(cached_riskiness_model(state, &runtime)),
                )
        });
        let mut value = projection.row_to_json(&runtime, &row, &statuses)?;
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
        "unavailable": unavailable,
    }))
}

/// One source `/api/tasks/all` could not read. The first recording wins so a
/// workspace with several unreadable rows is named once.
fn record_unavailable(
    unavailable: &mut Vec<Value>,
    recorded: &mut HashSet<String>,
    entry: &crate::state::WsEntry,
    error: &str,
) {
    if recorded.insert(entry.id.clone()) {
        unavailable.push(json!({
            "workspace_id": entry.id,
            "workspace_name": entry.name,
            "error": error,
        }));
    }
}

/// One workspace's candidates for the merge, as ids and creation times. A
/// filter the task index fully answers is selected there without reading a
/// single envelope, so only the merged page's bundles are ever opened; the
/// rest (title search, type, parent, external refs) select over envelopes.
///
/// The aggregate is observational — like `/api/job-runs/all`, a dashboard
/// poll costs reads only — so selection observes settled envelopes without
/// entering the task-commit boundary: it never queues behind a drain's
/// exclusive commit sections and never triggers recovery or index repair.
/// The `pilot_warnings` selection has no observational form and keeps the
/// coordinated read.
fn workspace_task_keys(
    runtime: &OrbitRuntime,
    query: &TaskPageQuery,
    filter: orbit_core::application::task::TaskListFilter,
    limit: usize,
) -> Result<TaskCandidateKeys, orbit_core::OrbitError> {
    if query.pilot_warnings() {
        let page = runtime.query_task_rows(&orbit_core::application::task::TaskListQuery {
            filter,
            pilot_warnings: true,
            limit,
            ..Default::default()
        })?;
        return Ok(TaskCandidateKeys {
            items: page
                .items
                .into_iter()
                .map(|row| TaskCandidateKey {
                    id: row.task.id,
                    created_at: row.task.created_at,
                })
                .collect(),
            total: page.total,
            total_without_cursor: page.total_without_cursor,
        });
    }
    if let Some(keys) = runtime.observe_task_candidate_keys(&filter, limit)? {
        return Ok(keys);
    }
    let page = runtime.observe_task_candidates(&filter, limit)?;
    Ok(TaskCandidateKeys {
        items: page
            .items
            .into_iter()
            .map(|task| TaskCandidateKey {
                id: task.id,
                created_at: task.created_at,
            })
            .collect(),
        total: page.total,
        total_without_cursor: page.total_without_cursor,
    })
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
