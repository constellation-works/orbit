//! `GET /api/tasks/all`: tasks merged across every registered workspace.

use std::collections::{BTreeMap, HashSet};

use axum::extract::{RawQuery, State};
use axum::response::{IntoResponse, Json, Response};
use orbit_core::OrbitRuntime;
use orbit_core::application::task::{TaskCandidateKey, TaskCandidateKeys};
use serde_json::{Value, json};

use super::super::pagination::TaskPageQuery;
use super::super::{bad_request, blocking, server_error};
use super::fan_out::fan_out;
use super::list::{abbreviate_home, entry_is_owned};
use crate::projections::{TaskListProjection, cached_riskiness_model};
use crate::state::{DashboardState, Pinned};

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
///
/// `group=status` and `status_limit` answer the per-status form documented on
/// `list_tasks`: each section is cut workspace by workspace like a plain page,
/// its cursor names the workspace it resumes in, and `unavailable` names each
/// unreadable workspace once across every section.
pub(in crate::api) async fn list_all_tasks(
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
        if query.groups_by_status() {
            return Ok(all_task_sections_json(&state, &pinned, &query, &scope));
        }
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

/// The `group=status` answer for `/api/tasks/all`: one aggregate page per
/// status, with the sources any section could not read named once.
fn all_task_sections_json(
    state: &DashboardState,
    pinned: &Pinned,
    query: &TaskPageQuery,
    scope: &str,
) -> Result<Value, orbit_core::OrbitError> {
    let mut sections = Vec::new();
    let mut total = 0_u64;
    let mut unavailable = Vec::new();
    let mut unavailable_ids = HashSet::new();
    for (status, section_query) in query.status_section_queries() {
        let mut section = all_tasks_json(state, pinned, &section_query, scope)?;
        if let Value::Object(map) = &mut section {
            if let Some(Value::Array(sources)) = map.remove("unavailable") {
                for source in sources {
                    let id = source["workspace_id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    if unavailable_ids.insert(id) {
                        unavailable.push(source);
                    }
                }
            }
            map.insert("status".to_string(), json!(status.cli_name()));
        }
        total = total.saturating_add(section["total"].as_u64().unwrap_or(0));
        sections.push(section);
    }
    Ok(json!({
        "group": "status",
        "sections": sections,
        "total": total,
        "limit": query.limit(),
        "unavailable": unavailable,
    }))
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
    let mut run_branches_by_workspace: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (row, runtime, entry) in &rows {
        let _ = row;
        if !attention_by_workspace.contains_key(&entry.id) {
            let workspace_rows = rows
                .iter()
                .filter(|(_, _, other)| other.id == entry.id)
                .map(|(row, _, _)| row);
            let map = runtime.delivery_attention_map(workspace_rows)?;
            attention_by_workspace.insert(entry.id.clone(), map);
            run_branches_by_workspace.insert(
                entry.id.clone(),
                runtime.local_run_branches(
                    rows.iter()
                        .filter(|(_, _, other)| other.id == entry.id)
                        .map(|(row, _, _)| row),
                ),
            );
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
                .with_run_branches(
                    run_branches_by_workspace
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
