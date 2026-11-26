//! `GET /api/tasks/all`: tasks merged across every registered workspace.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use axum::extract::{RawQuery, State};
use axum::response::{IntoResponse, Json, Response};
use orbit_core::Core;
use orbit_core::app::workspace::{CandidateSection, Command, Outcome, TaskRows, exec};
use orbit_core::application::task::{TaskCandidateKeys, TaskListFilter};
use serde_json::{Value, json};

use super::super::pagination::TaskPageQuery;
use super::super::{bad_request, blocking, server_error};
use super::fan_out::fan_out;
use super::list::{abbreviate_home, entry_is_owned};
use crate::projections::{TaskListProjection, cached_riskiness_model};
use crate::state::{DashboardState, Pinned, Ws, WsCore, WsEntry};

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
        let entries = aggregate_task_entries(&pinned, &query);
        let resume_index = aggregate_resume_index(&entries, &query)?;
        if query.groups_by_status() {
            return Ok(all_task_sections_json(
                &state,
                &pinned,
                &query,
                &scope,
                &entries,
                resume_index,
            ));
        }
        let selections = select_workspace_task_pages(&pinned, &entries, &[&query], resume_index);
        Ok(all_tasks_json(
            &state,
            &query,
            &scope,
            &entries,
            resume_index,
            selections.into_iter().next().unwrap_or_default(),
        ))
    })
    .await
    {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => server_error(error),
        Err(response) => *response,
    }
}

/// The workspaces `/api/tasks/all` reads, in `/api/workspaces` order.
fn aggregate_task_entries<'a>(pinned: &'a Pinned, query: &TaskPageQuery) -> Vec<&'a WsEntry> {
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
    entries: &[&WsEntry],
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
    entries: &[&WsEntry],
    resume_index: usize,
) -> Result<Value, orbit_core::OrbitError> {
    let section_queries = query.status_section_queries();
    let queries: Vec<_> = section_queries.iter().map(|(_, query)| query).collect();
    let selections = select_workspace_task_pages(pinned, entries, &queries, resume_index);
    let mut sections = Vec::new();
    let mut total = 0_u64;
    let mut unavailable = Vec::new();
    let mut unavailable_ids = HashSet::new();
    for ((status, section_query), pages) in section_queries.into_iter().zip(selections) {
        let mut section =
            all_tasks_json(state, &section_query, scope, entries, resume_index, pages)?;
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

type WorkspaceSelection = Result<(Arc<Source>, TaskCandidateKeys), String>;

/// One workspace an aggregate page reads: its app entry, and the runtime its
/// row projections still read.
struct Source {
    core: Arc<Core>,
    ws: Ws,
}

/// Visit each workspace once, then transpose the results into section order.
/// Within each section the workspace order and its individual errors remain
/// independent of the order in which the workers finish.
fn select_workspace_task_pages(
    pinned: &Pinned,
    entries: &[&WsEntry],
    queries: &[&TaskPageQuery],
    resume_index: usize,
) -> Vec<Vec<WorkspaceSelection>> {
    let Some(first) = queries.first() else {
        return Vec::new();
    };
    let batch_indexed = queries.len() > 1 && !first.pilot_warnings();
    let selections = fan_out(entries.len(), |index| {
        let entry = entries[index];
        if !entry.active {
            return Err("workspace is unavailable".to_string());
        }
        let runtime = pinned.runtime_for(&entry.id).map_err(|_| {
            tracing::warn!(workspace = %entry.id, "fleet task list could not open a workspace");
            "failed to open workspace".to_string()
        })?;
        let source = Arc::new(Source {
            core: WsCore::over(&runtime).0,
            ws: Ws(runtime),
        });
        let sections = queries
            .iter()
            .map(|query| {
                let (filter, limit) = workspace_task_filter(query, index, resume_index);
                CandidateSection {
                    filter,
                    limit,
                    pilot_warnings: !batch_indexed && query.pilot_warnings(),
                }
            })
            .collect();
        let pages = match exec(
            &source.core,
            Command::TaskCandidates {
                sections,
                batch: batch_indexed,
            },
        ) {
            Ok(Outcome::TaskCandidates(pages)) => pages,
            Ok(_) => unreachable!("fleet task selection returned an unrelated outcome"),
            Err(error) => {
                tracing::warn!(workspace = %entry.id, %error, "fleet task list skipped a workspace");
                return Err(error.to_string());
            }
        };
        Ok(pages
            .into_iter()
            .map(|page| {
                page.map(|page| (Arc::clone(&source), page))
                    .map_err(|error| {
                        tracing::warn!(workspace = %entry.id, %error, "fleet task list skipped a workspace");
                        error.to_string()
                    })
            })
            .collect::<Vec<_>>())
    });
    let mut by_section: Vec<_> = queries
        .iter()
        .map(|_| Vec::with_capacity(entries.len()))
        .collect();
    for selection in selections {
        match selection {
            Ok(pages) => {
                for (section, page) in by_section.iter_mut().zip(pages) {
                    section.push(page);
                }
            }
            Err(error) => {
                for section in &mut by_section {
                    section.push(Err(error.clone()));
                }
            }
        }
    }
    by_section
}

fn workspace_task_filter(
    query: &TaskPageQuery,
    index: usize,
    resume_index: usize,
) -> (TaskListFilter, usize) {
    match index.cmp(&resume_index) {
        // Only the total is read from a spent workspace.
        std::cmp::Ordering::Less => (query.filter_scanning_before(None), 1),
        std::cmp::Ordering::Equal => (query.filter(), query.limit()),
        std::cmp::Ordering::Greater => (query.filter_scanning_before(None), query.limit()),
    }
}

fn all_tasks_json(
    state: &DashboardState,
    query: &TaskPageQuery,
    scope: &str,
    entries: &[&WsEntry],
    resume_index: usize,
    selections: Vec<WorkspaceSelection>,
) -> Result<Value, orbit_core::OrbitError> {
    let home = orbit_common::fs::path::home_dir(&orbit_core::composition::composition_env()).ok();
    let mut candidates = Vec::new();
    let mut total = 0;
    let mut remaining = 0;
    let mut unavailable = Vec::new();
    let mut unavailable_ids = HashSet::new();
    for (index, (entry, selection)) in entries.iter().zip(selections).enumerate() {
        let (source, page) = match selection {
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
            candidates.push((task, Arc::clone(&source), *entry));
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
    // One read per workspace, in page order: its candidates' rows and one
    // batched delivery-run read for those that read.
    let mut groups: Vec<(Arc<Source>, &WsEntry, Vec<String>)> = Vec::new();
    for (task, source, entry) in candidates {
        match groups.last_mut() {
            Some((_, last, ids)) if last.id == entry.id => ids.push(task.id),
            _ => groups.push((source, entry, vec![task.id])),
        }
    }
    let mut rows = Vec::new();
    // In page order, so the first failed delivery-run read is the one
    // reported.
    let mut attention_by_workspace = Vec::new();
    let mut run_branches_by_workspace: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (source, entry, ids) in groups {
        let Outcome::TaskRows(read) = exec(&source.core, Command::TaskRows(ids.clone()))? else {
            unreachable!("fleet task rows returned an unrelated outcome");
        };
        let TaskRows {
            rows: read_rows,
            delivery_attention,
            run_branches,
        } = *read;
        let mut read_any = false;
        for (id, row) in ids.iter().zip(read_rows) {
            // Same contract as the per-workspace scan above: one unreadable
            // row is logged and skipped rather than failing the whole fleet
            // list.
            match row {
                Ok(Some(row)) => {
                    read_any = true;
                    rows.push((row, Arc::clone(&source), entry));
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(
                        workspace = %entry.id,
                        task = %id,
                        %error,
                        "fleet task list skipped an unreadable task row"
                    );
                    // The dropped row would look like a new review task when
                    // it returns, unless this read says the workspace was
                    // incomplete.
                    record_unavailable(
                        &mut unavailable,
                        &mut unavailable_ids,
                        entry,
                        &error.to_string(),
                    );
                }
            }
        }
        if read_any {
            attention_by_workspace.push((entry.id.clone(), delivery_attention));
            run_branches_by_workspace.insert(entry.id.clone(), run_branches);
        }
    }
    // All runtimes in a dashboard share one coordination registry. Read its
    // global dependency projection once, after hydrating the page.
    let statuses = match rows.first() {
        Some((_, source, _)) => {
            let tasks = rows.iter().map(|(row, _, _)| row.task.clone()).collect();
            match exec(&source.core, Command::DependencyStatuses(tasks))? {
                Outcome::DependencyStatuses(statuses) => statuses,
                _ => unreachable!("dependency statuses returned an unrelated outcome"),
            }
        }
        None => BTreeMap::new(),
    };
    let attention_by_workspace = attention_by_workspace
        .into_iter()
        .map(|(id, attention)| attention.map(|attention| (id, attention)))
        .collect::<Result<BTreeMap<String, BTreeMap<String, Value>>, _>>()?;
    let mut projections: BTreeMap<&str, TaskListProjection> = BTreeMap::new();
    let mut values = Vec::with_capacity(rows.len());
    for (row, source, entry) in rows {
        // The row projections still read the workspace's runtime.
        let runtime = &source.ws.0;
        let projection = projections.entry(entry.id.as_str()).or_insert_with(|| {
            TaskListProjection::new(runtime)
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
                        .block_on(cached_riskiness_model(state, runtime)),
                )
        });
        let mut value = projection.row_to_json(runtime, &row, &statuses)?;
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
    entry: &WsEntry,
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
