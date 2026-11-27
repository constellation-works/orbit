//! `GET /api/tasks/all`: tasks merged across every registered workspace.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{RawQuery, State};
use axum::response::{IntoResponse, Json, Response};
use orbit_core::Core;
use orbit_core::app::task;
use orbit_core::app::workspace::{
    self, AggregateTaskPage, AggregateTaskPages, AggregateTaskRequest, AggregateTaskRow,
    AggregateTaskSection, AggregateWorkspace, AggregateWorkspaceSource, Command, Outcome,
    UnavailableWorkspace,
};
use orbit_types::task::TaskStatus;
use serde_json::{Value, json};

use super::fan_out::fan_out;
use super::list::{abbreviate_home, entry_is_owned};
use crate::api::pagination::TaskPageQuery;
use crate::api::{bad_request, blocking, server_error};
use crate::projections::cached_riskiness_model;
use crate::state::{DashboardState, Pinned, Ws, WsCore, WsEntry};

/// One workspace an aggregate page opened: its app entry, and the runtime its
/// row projections still read.
struct Source {
    core: Arc<Core>,
    ws: Ws,
}

/// Shared rendering state for one `/api/tasks/all` response.
struct RenderContext<'a> {
    state: &'a DashboardState,
    scope: &'a str,
    entries: &'a [&'a WsEntry],
    opened: &'a [Result<Source, String>],
    home: Option<PathBuf>,
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
            .map_err(orbit_common::OrbitError::InvalidInput)?;
        let entries = aggregate_task_entries(&pinned, &query);
        let resume_index = aggregate_resume_index(&entries, &query)?;
        Ok(aggregate_tasks_json(
            &state,
            &pinned,
            &query,
            &scope,
            &entries,
            resume_index,
        ))
    })
    .await
    {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => server_error(error),
        Err(response) => *response,
    }
}

fn aggregate_tasks_json(
    state: &DashboardState,
    pinned: &Pinned,
    query: &TaskPageQuery,
    scope: &str,
    entries: &[&WsEntry],
    resume_index: usize,
) -> Result<Value, orbit_common::OrbitError> {
    let home = orbit_common::fs::path::home_dir(&orbit_core::app::host::composition_env()).ok();
    if query.groups_by_status() {
        let section_queries = query.status_section_queries();
        if section_queries.is_empty() {
            return Ok(json!({
                "group": "status",
                "sections": [],
                "total": 0,
                "limit": query.limit(),
                "unavailable": [],
            }));
        }
        let opened = open_workspaces(pinned, entries);
        let sections = section_queries
            .iter()
            .map(|(_, section_query)| section_spec(section_query))
            .collect();
        let pages = fetch_aggregate_pages(entries, &opened, sections, resume_index)?;
        let context = RenderContext {
            state,
            scope,
            entries,
            opened: &opened,
            home,
        };
        return render_grouped_pages(&context, query, section_queries, pages);
    }
    let opened = open_workspaces(pinned, entries);
    let pages = fetch_aggregate_pages(entries, &opened, vec![section_spec(query)], resume_index)?;
    let page = pages.sections.into_iter().next().ok_or_else(|| {
        orbit_common::OrbitError::Execution("aggregate task list answered no page".to_string())
    })?;
    let context = RenderContext {
        state,
        scope,
        entries,
        opened: &opened,
        home,
    };
    render_task_page(&context, query, page)
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
) -> Result<usize, orbit_common::OrbitError> {
    let Some(cursor) = query.cursor() else {
        return Ok(0);
    };
    let workspace_id = cursor.workspace_id.as_deref().ok_or_else(|| {
        orbit_common::OrbitError::InvalidInput(
            "task cursor predates workspace-ordered pages; reload the list".to_string(),
        )
    })?;
    entries
        .iter()
        .position(|entry| entry.id == workspace_id)
        .ok_or_else(|| {
            orbit_common::OrbitError::InvalidInput(
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

fn open_workspaces(pinned: &Pinned, entries: &[&WsEntry]) -> Vec<Result<Source, String>> {
    fan_out(entries.len(), |index| {
        let entry = entries[index];
        if !entry.active {
            return Err("workspace is unavailable".to_string());
        }
        let runtime = pinned.runtime_for(&entry.id).map_err(|_| {
            tracing::warn!(workspace = %entry.id, "fleet task list could not open a workspace");
            "failed to open workspace".to_string()
        })?;
        Ok(Source {
            core: WsCore::over(&runtime, pinned.session()).0,
            ws: Ws(runtime),
        })
    })
}

fn section_spec(query: &TaskPageQuery) -> AggregateTaskSection {
    AggregateTaskSection {
        filter: query.filter(),
        limit: query.limit(),
        pilot_warnings: query.pilot_warnings(),
    }
}

/// Run the one aggregate workspace app operation for this request, or return
/// empty pages naming every unavailable workspace when no workspace opened.
fn fetch_aggregate_pages(
    entries: &[&WsEntry],
    opened: &[Result<Source, String>],
    sections: Vec<AggregateTaskSection>,
    resume_index: usize,
) -> Result<AggregateTaskPages, orbit_common::OrbitError> {
    let Some((bound_index, bound_core)) = opened.iter().enumerate().find_map(|(index, slot)| {
        slot.as_ref()
            .ok()
            .map(|source| (index, source.core.as_ref()))
    }) else {
        return Ok(unavailable_pages(opened, sections.len()));
    };
    let workspaces = entries
        .iter()
        .zip(opened)
        .enumerate()
        .map(|(index, (entry, slot))| AggregateWorkspace {
            id: entry.id.as_str(),
            source: match slot {
                Ok(_) if index == bound_index => AggregateWorkspaceSource::Bound,
                Ok(source) => AggregateWorkspaceSource::Peer(source.core.as_ref()),
                Err(error) => AggregateWorkspaceSource::Unavailable(error.clone()),
            },
        })
        .collect();
    workspace::exec(
        bound_core,
        Command::AggregateTasks(AggregateTaskRequest {
            workspaces,
            sections,
            resume_index,
        }),
    )
    .and_then(Outcome::into_aggregate_tasks)
}

fn unavailable_pages(
    opened: &[Result<Source, String>],
    section_count: usize,
) -> AggregateTaskPages {
    let unavailable: Vec<UnavailableWorkspace> = opened
        .iter()
        .enumerate()
        .filter_map(|(workspace_index, slot)| {
            slot.as_ref().err().map(|error| UnavailableWorkspace {
                workspace_index,
                error: error.clone(),
            })
        })
        .collect();
    let sections = (0..section_count)
        .map(|_| AggregateTaskPage {
            rows: Vec::new(),
            total: 0,
            candidate_count: 0,
            next_cursor: None,
            delivery_attention: BTreeMap::new(),
            run_branches: BTreeMap::new(),
            dependency_statuses: BTreeMap::new(),
            unavailable: unavailable.clone(),
        })
        .collect();
    AggregateTaskPages { sections }
}

/// Render the `group=status` answer for `/api/tasks/all`: one aggregate page
/// per status, with the sources any section could not read named once.
fn render_grouped_pages(
    context: &RenderContext<'_>,
    query: &TaskPageQuery,
    section_queries: Vec<(TaskStatus, TaskPageQuery)>,
    pages: AggregateTaskPages,
) -> Result<Value, orbit_common::OrbitError> {
    let mut sections = Vec::with_capacity(section_queries.len());
    let mut total = 0_u64;
    let mut unavailable = Vec::new();
    let mut unavailable_ids = HashSet::new();
    for ((status, section_query), page) in section_queries.into_iter().zip(pages.sections) {
        let mut section = render_task_page(context, &section_query, page)?;
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

fn render_task_page(
    context: &RenderContext<'_>,
    query: &TaskPageQuery,
    page: AggregateTaskPage,
) -> Result<Value, orbit_common::OrbitError> {
    let offset = query.offset();
    let next_offset = offset.saturating_add(page.candidate_count);
    let next_cursor = page
        .next_cursor
        .as_ref()
        .map(|anchor| {
            query.next_workspace_cursor(
                context.scope,
                &context.entries[anchor.workspace_index].id,
                anchor.created_at,
                anchor.task_id.clone(),
                next_offset,
            )
        })
        .transpose()
        .map_err(|error| {
            orbit_common::OrbitError::Execution(format!("encode task cursor: {error}"))
        })?;
    let total = page.total;
    let unavailable: Vec<Value> = page
        .unavailable
        .iter()
        .map(|item| {
            let entry = context.entries[item.workspace_index];
            json!({
                "workspace_id": entry.id,
                "workspace_name": entry.name,
                "error": item.error,
            })
        })
        .collect();
    let values = project_rows(context, page)?;
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

fn project_rows(
    context: &RenderContext<'_>,
    mut page: AggregateTaskPage,
) -> Result<Vec<Value>, orbit_common::OrbitError> {
    // Rows are rendered a workspace at a time, each by its own task app, and
    // set back in page order.
    let mut by_workspace: BTreeMap<usize, (Vec<usize>, Vec<_>)> = BTreeMap::new();
    for (position, row) in page.rows.into_iter().enumerate() {
        let AggregateTaskRow {
            workspace_index,
            row,
        } = row;
        let (positions, rows) = by_workspace.entry(workspace_index).or_default();
        positions.push(position);
        rows.push(row);
    }
    let mut values = Vec::new();
    for (workspace_index, (positions, rows)) in by_workspace {
        let entry = context.entries[workspace_index];
        let source = context.opened[workspace_index].as_ref().map_err(|error| {
            orbit_common::OrbitError::Execution(format!(
                "missing opened workspace for hydrated row: {error}"
            ))
        })?;
        // On the blocking pool: wait for this workspace's model as the
        // per-workspace list does.
        let riskiness = tokio::runtime::Handle::current()
            .block_on(cached_riskiness_model(context.state, &source.ws.0));
        let rendered = task::exec(
            &source.core,
            task::Command::Read(task::Read::Rows(Box::new(task::RowsRead {
                rows,
                dependency_statuses: page.dependency_statuses.clone(),
                delivery_attention: page
                    .delivery_attention
                    .remove(&workspace_index)
                    .unwrap_or_default(),
                run_branches: page
                    .run_branches
                    .remove(&workspace_index)
                    .unwrap_or_default(),
                riskiness,
            }))),
        )
        .and_then(task::Outcome::into_rows)?;
        for (position, mut value) in positions.into_iter().zip(rendered) {
            if let Value::Object(map) = &mut value {
                map.insert("workspace_id".to_string(), json!(entry.id));
                map.insert("workspace_name".to_string(), json!(entry.name));
                map.insert(
                    "workspace_root".to_string(),
                    json!(abbreviate_home(&entry.repo_root, context.home.as_deref())),
                );
            }
            values.push((position, value));
        }
    }
    values.sort_by_key(|(position, _)| *position);
    Ok(values.into_iter().map(|(_, value)| value).collect())
}
