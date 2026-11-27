//! Task list, detail, locks and completion reads.

use crate::api::helpers;
use crate::api::pagination::TaskPageQuery;
use crate::api::{bad_request, blocking, server_error, validate_id};
use crate::projections::cached_riskiness_model;
use crate::state::{DashboardState, Ws, WsCore, WsRejection};
use axum::extract::{Path, RawQuery, State};
use axum::response::{IntoResponse, Json, Response};
use orbit_core::app::Session;
use orbit_core::app::task::TaskListQuery;
use orbit_core::app::task::{
    self, Command, Outcome, PageRead, PageSelect, PageSelection, Read, RowRead, TaskPages,
};
use serde_json::{Value, json};
use std::sync::Arc;

/// The workspace that owns a task, with the row read from it.
struct TaskOwner {
    id: String,
    name: String,
    root: String,
    ws: Ws,
    read: Box<RowRead>,
}

/// `GET /api/tasks` — the request workspace's tasks, filtered server-side.
///
/// ## Query parameters
///
/// - `status` — repeatable and/or comma-separated (`?status=proposed,backlog`).
///   OR semantics across values; omitted means every lifecycle status.
/// - `tag` (alias `tags`) — repeatable and/or comma-separated. AND semantics:
///   a task must carry every requested tag. Values go through Orbit's own
///   [`normalize_task_tags`](orbit_types::task::normalize_task_tags) /
///   `task_matches_tags`, so a colon is ordinary tag content and
///   `auto-task:qa-sweep` matches that whole tag rather than being split.
/// - `type` (alias `task_type`) — a single task type (`feature`/`bug`/
///   `refactor`/`chore`).
/// - `q` (alias `search`) — case-insensitive task ID/title substring.
/// - `detail` — optionally include the full projection for a task on this page.
/// - `detail_candidate=true` — with `detail`, include discard availability.
/// - `detail_runs=true` — with `detail`, include its bounded Runs tab page.
/// - `limit` — positive integer, defaulting to [`orbit_types::task::DEFAULT_TASK_LIST_LIMIT`].
/// - `cursor` — opaque continuation returned as `next_cursor`; it is rejected
///   when malformed or reused with a different workspace, endpoint, or filter.
///
/// Unknown keys (including the `?workspace=<id>` selector consumed by the
/// [`Ws`] extractor) are ignored; an unparseable value is a 400 rather than a
/// silently-dropped filter.
///
/// ## Response contract (ORB-10400, consumed by bridge ORB-10398)
///
/// ```json
/// { "items": [ /* task summary rows, newest first */ ],
///   "total": 137, "limit": 50, "truncated": true,
///   "offset": 0, "next_cursor": "...", "detail": { /* when requested */ } }
/// ```
///
/// Each item is a *summary* row (`"projection": "summary"`): the task's
/// scalar and short-list fields, `comment_count` / `history_count` /
/// `artifact_count` in place of the comment, history and artifact bodies, no
/// `description` / `plan` / `execution_summary` / `acceptance_criteria`, the
/// governed `status_transitions` without their `required_field`, a
/// `resolved_crew` read from the crew registry alone, and `job_run_navigable`
/// from the same local-machine decision as the detail projection.
/// `GET /api/tasks/:id`
/// serves the full projection (DANI-10391; see
/// [`TaskListProjection`]).
///
/// **Every predicate is applied before the limit**, so `items` holds the newest
/// *matching* tasks — a match older than the newest `limit` unfiltered tasks is
/// still reachable by passing its filters. `total` is the pre-limit match count
/// and `truncated` is `total > items.len()`, which is what lets a client tell a
/// genuinely empty result (`total: 0`) from a filter whose matches fell outside
/// the window (previously indistinguishable, because the handler answered a bare
/// truncated array with no metadata).
///
/// The cursor is an opaque continuation of the stable `created_at DESC, id ASC`
/// order and is bound to this workspace and the active filters. Inserts newer
/// than the first page do not disturb an existing continuation. Changing a
/// task's `created_at`, ID, or filter membership while paging may move it across
/// the boundary; clients that need a new live snapshot restart without a cursor.
/// The cross-workspace `/api/tasks/all` aggregate shares the filters and the
/// envelope, but not this order: it cuts pages workspace by workspace in
/// `/api/workspaces` order, and its cursor names the workspace it resumes in
/// (see `list_all_tasks` in `api/workspaces/all_tasks.rs`).
///
/// ## Per-status form (`group=status`)
///
/// With `group=status` the read answers one section per requested status
/// (request order; every status when `status` is omitted) instead of one page
/// across them, so a status never splits across pages:
///
/// ```json
/// { "group": "status", "total": 212, "limit": 25,
///   "sections": [ { "status": "backlog", "items": [ … ], "total": 180,
///                   "limit": 25, "offset": 0, "truncated": true,
///                   "next_cursor": "..." } ],
///   "detail": { /* when requested and on a section's rows */ } }
/// ```
///
/// A section's `total` counts every task in that status under the request's
/// filters, whether or not its rows are loaded, and it is listed even at 0.
/// `limit` is each section's first-read size; `status_limit=<status>:<n>`
/// (repeatable or comma-separated) overrides it per status. A section's
/// `next_cursor` continues that status alone: pass it back with
/// `group=status`, that one `status`, the same filters and any `limit`. The
/// form is opt-in; a read without `group` keeps the envelope above.
pub(in crate::api) async fn list_tasks(
    State(state): State<DashboardState>,
    ws: Ws,
    RawQuery(query): RawQuery,
) -> Response {
    let query = match TaskPageQuery::parse(query.as_deref()) {
        Ok(query) => query,
        Err(message) => return bad_request(message),
    };
    if let Some(id) = query.detail_id()
        && let Err(message) = validate_id(id)
    {
        return bad_request(message);
    }
    let riskiness = cached_riskiness_model(&state, &ws.0).await;
    let session = state.session();
    let WsCore(core) = WsCore::over(&ws.0, session);
    match blocking("task list", move || {
        let select = query.clone();
        let read = Read::Pages {
            select: PageSelect::new(move |workspace_id| select_pages(select, workspace_id)),
            detail: query.detail_id().map(ToOwned::to_owned),
            riskiness,
            detail_candidate: query.detail_candidate,
        };
        let TaskPages {
            workspace_id,
            pages,
        } = task::exec(&core, Command::Read(read)).and_then(Outcome::into_pages)?;
        let scope = workspace_scope(&workspace_id);
        let mut query = query;
        query
            .bind_cursor(&scope)
            .map_err(orbit_common::OrbitError::InvalidInput)?;
        Ok(pages.and_then(|pages| {
            if query.groups_by_status() {
                status_sections_json((&ws, session), &query, &scope, pages)
            } else {
                task_list_page_json((&ws, session), &query, &scope, pages)
            }
        }))
    })
    .await
    {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(e)) => server_error(e),
        Err(response) => *response,
    }
}

/// The cursor scope of one workspace's task pages.
fn workspace_scope(workspace_id: &str) -> String {
    format!("workspace:{workspace_id}")
}

/// Name the pages `query` asks of the workspace: one page per status for
/// `group=status`, selected in one batch so the request validates the task
/// index's freshness once rather than once per section, else one page.
fn select_pages(
    mut query: TaskPageQuery,
    workspace_id: &str,
) -> Result<PageSelection, orbit_common::OrbitError> {
    query
        .bind_cursor(&workspace_scope(workspace_id))
        .map_err(orbit_common::OrbitError::InvalidInput)?;
    if query.groups_by_status() {
        return Ok(PageSelection::Sections {
            query: TaskListQuery {
                pilot_warnings: query.pilot_warnings(),
                ..Default::default()
            },
            sections: query
                .status_section_queries()
                .iter()
                .map(|(_, section)| (section.filter(), section.limit()))
                .collect(),
        });
    }
    Ok(PageSelection::One(TaskListQuery {
        filter: query.filter(),
        pilot_warnings: query.pilot_warnings(),
        limit: query.limit(),
        ..Default::default()
    }))
}

/// Build the `group=status` answer for `GET /api/tasks`: one
/// [`task_page_json`] page per status, each bound to that status alone.
fn status_sections_json(
    ws: (&Ws, Session),
    query: &TaskPageQuery,
    scope: &str,
    pages: Vec<PageRead>,
) -> Result<Value, orbit_common::OrbitError> {
    let mut result = json!({ "group": "status", "limit": query.limit() });
    let mut sections = Vec::new();
    let mut total = 0_u64;
    for ((status, section_query), page) in query.status_section_queries().into_iter().zip(pages) {
        let mut section = task_page_json(ws, &section_query, scope, page)?;
        if let Value::Object(map) = &mut section {
            // The selected task's detail sits on whichever section holds it.
            for key in ["detail", "detail_runs", "detail_runs_error"] {
                if let Some(value) = map.remove(key) {
                    result[key] = value;
                }
            }
            map.insert("status".to_string(), json!(status.cli_name()));
        }
        total = total.saturating_add(section["total"].as_u64().unwrap_or(0));
        sections.push(section);
    }
    result["sections"] = json!(sections);
    result["total"] = json!(total);
    Ok(result)
}

/// Build the `{ items, total, limit, truncated }` page for `GET /api/tasks`.
fn task_list_page_json(
    ws: (&Ws, Session),
    query: &TaskPageQuery,
    scope: &str,
    pages: Vec<PageRead>,
) -> Result<Value, orbit_common::OrbitError> {
    let page = pages.into_iter().next().ok_or_else(|| {
        orbit_common::OrbitError::Execution("task list read answered no page".to_string())
    })?;
    task_page_json(ws, query, scope, page)
}

/// Project one selected page of `query` into its response envelope.
fn task_page_json(
    (ws, session): (&Ws, Session),
    query: &TaskPageQuery,
    scope: &str,
    read: PageRead,
) -> Result<Value, orbit_common::OrbitError> {
    let PageRead {
        page,
        items,
        detail,
    } = read;
    let total = page.total_without_cursor;
    let offset = query.offset();
    let next_offset = offset.saturating_add(items.len());
    let next_cursor = if page.total > items.len() {
        page.items
            .last()
            .map(|row| {
                query.next_cursor(scope, row.task.created_at, row.task.id.clone(), next_offset)
            })
            .transpose()
            .map_err(|error| {
                orbit_common::OrbitError::Execution(format!("encode task cursor: {error}"))
            })?
    } else {
        None
    };
    let mut result = json!({
        "truncated": total > items.len(),
        "items": items,
        "total": total,
        "limit": query.limit(),
        "offset": offset,
        "next_cursor": next_cursor,
    });
    if let Some(detail_id) = query.detail_id()
        && let Some(detail) = detail
    {
        match detail {
            Ok(detail) => result["detail"] = detail,
            Err(error) => tracing::debug!(
                task_id = %detail_id,
                %error,
                "task list detail projection unavailable; dashboard will read it separately"
            ),
        }
        if query.include_detail_runs() {
            match crate::api::jobs::task_runs_page(&WsCore::over(&ws.0, session).0, detail_id, 20) {
                Ok(runs) => result["detail_runs"] = runs,
                Err(error) => {
                    tracing::debug!(task_id = %detail_id, %error, "task list Runs projection unavailable");
                    result["detail_runs_error"] = json!("Task run history is unavailable");
                }
            }
        }
    }
    Ok(result)
}

pub(in crate::api) async fn list_task_locks(WsCore(core): WsCore) -> Response {
    match blocking("task locks", move || {
        Ok(task::exec(&core, Command::Read(Read::Locks)).and_then(Outcome::into_answer))
    })
    .await
    {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(e)) => server_error(e),
        Err(response) => *response,
    }
}

/// One task's row and the statuses of its dependencies, read through the
/// task app.
fn read_task_row(
    ws: &Ws,
    session: Session,
    id: &str,
) -> Result<Box<RowRead>, orbit_common::OrbitError> {
    let WsCore(core) = WsCore::over(&ws.0, session);
    task::exec(&core, Command::Read(Read::Row { id: id.to_string() })).and_then(Outcome::into_row)
}

/// `GET /api/tasks/{id}` — one task's full projection.
///
/// `?workspace=<id>` reads that workspace alone and never reaches into
/// another. `?workspace=all` finds the workspace that owns `id` (a task id is
/// unique on a host) and answers its detail with `workspace_id`,
/// `workspace_name` and `workspace_root` stamped on, as the aggregate list
/// rows carry them, so the page scopes follow-up reads and actions to the
/// owner. An id no active workspace owns is the same 404 as a scoped miss.
pub(in crate::api) async fn get_task(
    State(state): State<DashboardState>,
    ws: Result<Ws, WsRejection>,
    RawQuery(query): RawQuery,
    Path(id): Path<String>,
) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id.to_string(),
        Err(message) => return bad_request(message),
    };
    let include_candidate = has_query_value(query.as_deref(), "detail_candidate", "true");
    if has_query_value(query.as_deref(), "workspace", "all") {
        let lookup_state = state.clone();
        let lookup_id = id.clone();
        let owner = match blocking("task owner lookup", move || {
            find_task_owner(&lookup_state, &lookup_id)
        })
        .await
        {
            Ok(owner) => owner,
            Err(response) => return *response,
        };
        let TaskOwner {
            id,
            name,
            root,
            ws,
            read,
        } = owner;
        return task_detail_response(&state, ws, read, Some((id, name, root)), include_candidate)
            .await;
    }
    let ws = match ws {
        Ok(ws) => ws,
        Err(rejection) => return rejection.into_response(),
    };
    let lookup = Ws(Arc::clone(&ws.0));
    let session = state.session();
    let read = match blocking("task detail", move || read_task_row(&lookup, session, &id)).await {
        Ok(read) => read,
        Err(response) => return *response,
    };
    task_detail_response(&state, ws, read, None, include_candidate).await
}

fn has_query_value(query: Option<&str>, name: &str, expected: &str) -> bool {
    query.is_some_and(|query| {
        url::form_urlencoded::parse(query.as_bytes())
            .any(|(key, value)| key == name && value == expected)
    })
}

/// Find the owned workspace that owns task `id`, in registry order.
///
/// Replica checkouts are never owners: the All workspaces aggregate covers
/// only owned workspaces, so a replica is skipped the same way an inactive
/// entry is. A workspace that cannot be opened or read is skipped with a
/// warning; if no owner is found and one was skipped, its error is returned
/// instead of a not-found, because the id may live in the workspace that
/// could not be read.
fn find_task_owner(
    state: &DashboardState,
    id: &str,
) -> Result<TaskOwner, orbit_common::OrbitError> {
    let pinned = state.pin();
    let home = orbit_common::fs::path::home_dir(&orbit_core::app::host::composition_env()).ok();
    let mut skipped: Option<orbit_common::OrbitError> = None;
    for entry in pinned
        .entries()
        .iter()
        .filter(|entry| entry.active)
        .filter(|entry| crate::api::workspaces::entry_is_owned(entry))
    {
        let ws = match pinned.runtime_for(&entry.id) {
            Ok(runtime) => Ws(runtime),
            Err(_) => {
                tracing::warn!(workspace = %entry.id, "task owner lookup could not open workspace");
                skipped.get_or_insert_with(|| {
                    orbit_common::OrbitError::Execution(format!(
                        "failed to open workspace '{}'",
                        entry.id
                    ))
                });
                continue;
            }
        };
        match read_task_row(&ws, pinned.session(), id) {
            Ok(read) => {
                return Ok(TaskOwner {
                    id: entry.id.clone(),
                    name: entry.name.clone(),
                    root: crate::api::workspaces::abbreviate_home(
                        &entry.repo_root,
                        home.as_deref(),
                    ),
                    ws,
                    read,
                });
            }
            Err(orbit_common::OrbitError::NotFound {
                kind: orbit_common::NotFoundKind::Task,
                ..
            }) => {}
            Err(error) => {
                tracing::warn!(workspace = %entry.id, %error, "task owner lookup could not read workspace");
                skipped.get_or_insert(error);
            }
        }
    }
    Err(skipped.unwrap_or_else(|| {
        orbit_common::OrbitError::not_found(orbit_common::NotFoundKind::Task, id.to_string())
    }))
}

/// Render the read row's full projection from its workspace, stamping the
/// owning workspace's `(id, name, root)` on it when the read resolved through
/// the aggregate.
async fn task_detail_response(
    state: &DashboardState,
    ws: Ws,
    read: Box<RowRead>,
    stamp: Option<(String, String, String)>,
    include_candidate: bool,
) -> Response {
    let riskiness = cached_riskiness_model(state, &ws.0).await;
    let session = state.session();
    match blocking("task detail", move || {
        let WsCore(core) = WsCore::over(&ws.0, session);
        let mut value = task::exec(
            &core,
            Command::Read(Read::Detail {
                read,
                riskiness,
                include_candidate,
            }),
        )
        .and_then(Outcome::into_answer)?;
        if let (Some((id, name, root)), Value::Object(map)) = (stamp, &mut value) {
            map.insert("workspace_id".to_string(), json!(id));
            map.insert("workspace_name".to_string(), json!(name));
            map.insert("workspace_root".to_string(), json!(root));
        }
        Ok(value)
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

/// Status distribution per complexity bucket, including the explicit `unset`
/// band. Counts come from the generated task index — no per-request YAML reads.
pub(in crate::api) async fn completion_by_complexity(WsCore(core): WsCore) -> Response {
    let rows = match helpers::blocking_join("completion-by-complexity aggregation", move || {
        task::exec(&core, Command::Read(Read::CompletionByComplexity))
            .and_then(Outcome::into_completion)
    })
    .await
    {
        Ok(Ok(rows)) => rows,
        Ok(Err(e)) => return server_error(e),
        Err(response) => return *response,
    };

    const REQUIRED_STATUSES: &[&str] = &["done", "rejected", "archived"];
    let by_complexity: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            let mut statuses = Vec::new();
            let mut seen = std::collections::BTreeSet::new();
            for status in REQUIRED_STATUSES {
                let count = row.by_status.get(*status).copied().unwrap_or(0);
                statuses.push(status_rate_json(status, count, row.total));
                seen.insert((*status).to_string());
            }
            for (status, count) in &row.by_status {
                if seen.contains(status) {
                    continue;
                }
                statuses.push(status_rate_json(status, *count, row.total));
            }
            json!({
                "complexity": row.complexity,
                "total": row.total,
                "statuses": statuses,
            })
        })
        .collect();

    Json(json!({ "by_complexity": by_complexity })).into_response()
}

fn status_rate_json(status: &str, count: i64, total: i64) -> Value {
    let rate = if total > 0 {
        count as f64 / total as f64
    } else {
        0.0
    };
    json!({
        "status": status,
        "count": count,
        "total": total,
        "rate": rate,
    })
}
