//! Task list, detail, locks and completion reads.

use crate::api::helpers;
use crate::api::pagination::TaskPageQuery;
use crate::api::{bad_request, blocking, server_error, validate_id};
use crate::projections::{TaskListProjection, cached_riskiness_model, task_detail_json};
use crate::state::{DashboardState, Ws, WsRejection};
use axum::extract::{Path, RawQuery, State};
use axum::response::{IntoResponse, Json, Response};
use orbit_core::application::task::DashboardPageReads;
use orbit_core::{OrbitRuntime, Task, TaskStatus};
use serde_json::{Value, json};
use std::sync::Arc;

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
/// - `detail_runs=true` — with `detail`, include its bounded Runs tab page.
/// - `limit` — positive integer, defaulting to [`orbit_core::DEFAULT_TASK_LIST_LIMIT`].
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
    Ws(runtime): Ws,
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
    let riskiness = cached_riskiness_model(&state, &runtime).await;
    match blocking("task list", move || {
        let scope = format!("workspace:{}", runtime.workspace_id()?);
        let mut query = query;
        query
            .bind_cursor(&scope)
            .map_err(orbit_core::OrbitError::InvalidInput)?;
        if query.groups_by_status() {
            return Ok(status_sections_json(&runtime, &query, &scope, riskiness));
        }
        Ok(task_list_page_json(&runtime, &query, &scope, riskiness))
    })
    .await
    {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(e)) => server_error(e),
        Err(response) => *response,
    }
}

/// Build the `group=status` answer for `GET /api/tasks`: one
/// [`task_page_json`] page per status, each bound to that status alone. The
/// sections are selected in one batch, so the request validates the task
/// index's freshness once rather than once per section.
fn status_sections_json(
    runtime: &OrbitRuntime,
    query: &TaskPageQuery,
    scope: &str,
    riskiness: Option<Arc<orbit_core::application::codebase::TaskRiskinessModel>>,
) -> Result<Value, orbit_core::OrbitError> {
    let mut result = json!({ "group": "status", "limit": query.limit() });
    let section_queries = query.status_section_queries();
    let filters = section_queries
        .iter()
        .map(|(_, section)| (section.filter(), section.limit()))
        .collect::<Vec<_>>();
    let pages = runtime.query_task_row_pages(
        &orbit_core::application::task::TaskListQuery {
            pilot_warnings: query.pilot_warnings(),
            ..Default::default()
        },
        &filters,
    )?;
    let mut sections = Vec::new();
    let mut total = 0_u64;
    for ((status, section_query), page) in section_queries.into_iter().zip(pages) {
        let mut section = task_page_json(runtime, &section_query, scope, riskiness.clone(), page)?;
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
    runtime: &OrbitRuntime,
    query: &TaskPageQuery,
    scope: &str,
    riskiness: Option<Arc<orbit_core::application::codebase::TaskRiskinessModel>>,
) -> Result<Value, orbit_core::OrbitError> {
    let page = runtime.query_task_rows(&orbit_core::application::task::TaskListQuery {
        filter: query.filter(),
        pilot_warnings: query.pilot_warnings(),
        limit: query.limit(),
        ..Default::default()
    })?;
    task_page_json(runtime, query, scope, riskiness, page)
}

/// Project one selected page of `query` into its response envelope.
fn task_page_json(
    runtime: &OrbitRuntime,
    query: &TaskPageQuery,
    scope: &str,
    riskiness: Option<Arc<orbit_core::application::codebase::TaskRiskinessModel>>,
    page: orbit_core::application::task::TaskPage,
) -> Result<Value, orbit_core::OrbitError> {
    let total = page.total_without_cursor;
    let status_by_id = page.status_by_id;
    let attention = runtime.delivery_attention_map(page.items.iter())?;
    let detail_riskiness = riskiness.clone();
    let projection = TaskListProjection::new(runtime)
        .with_delivery_attention(attention)
        .with_run_branches(runtime.local_run_branches(page.items.iter()))
        .with_riskiness(riskiness);
    let items = page
        .items
        .iter()
        .map(|row| projection.row_to_json(runtime, row, &status_by_id))
        .collect::<Result<Vec<_>, _>>()?;
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
                orbit_core::OrbitError::Execution(format!("encode task cursor: {error}"))
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
        && let Some(row) = page.items.iter().find(|row| row.task.id == detail_id)
    {
        // The page's rows already read the workspace-wide triage, pilot
        // preparation, pull-request links and delivery attention this detail
        // shows; it borrows them rather than reading each again [ORB-21825].
        let page_reads = projection.detail_page_reads();
        match dashboard_status_index(runtime, &row.task).and_then(|statuses| {
            task_detail_json(
                runtime,
                row,
                &statuses,
                detail_riskiness.as_deref(),
                &page_reads,
            )
        }) {
            Ok(detail) => result["detail"] = detail,
            Err(error) => tracing::debug!(
                task_id = %detail_id,
                %error,
                "task list detail projection unavailable; dashboard will read it separately"
            ),
        }
        if query.include_detail_runs() {
            match super::super::jobs::task_runs_page(runtime, detail_id, 20) {
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

pub(in crate::api) async fn list_task_locks(Ws(runtime): Ws) -> Response {
    match blocking("task locks", move || Ok(runtime.task_locks_snapshot())).await {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(e)) => server_error(e),
        Err(response) => *response,
    }
}

/// Dependency-status projection for dashboard task serialization.
///
/// Uses the coordination registry's global status index
/// ([`OrbitRuntime::task_status_index`]) rather than a workspace-scoped
/// task list, so a task depending on another registered workspace's
/// task resolves that dependency's real status instead of `[missing]`
/// (ORB-10291). Task *listing* stays workspace-scoped: this index is only
/// consulted to label dependencies, never to add tasks to the response body.
/// `task`'s archived dependencies that reached `done` first label as `done`
/// ([`OrbitRuntime::dependency_status_index`]).
fn dashboard_status_index(
    runtime: &OrbitRuntime,
    task: &Task,
) -> Result<std::collections::BTreeMap<String, TaskStatus>, orbit_core::OrbitError> {
    runtime.dependency_status_index([task])
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
    if workspace_is_all(query.as_deref()) {
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
            runtime,
            row,
        } = owner;
        return task_detail_response(&state, runtime, row, Some((id, name, root))).await;
    }
    let Ws(runtime) = match ws {
        Ok(ws) => ws,
        Err(rejection) => return rejection.into_response(),
    };
    let lookup = Arc::clone(&runtime);
    let row = match blocking("task detail", move || lookup.get_task_row(&id)).await {
        Ok(row) => row,
        Err(response) => return *response,
    };
    task_detail_response(&state, runtime, row, None).await
}

/// The workspace that owns a task, with the row read from it.
struct TaskOwner {
    id: String,
    name: String,
    root: String,
    runtime: Arc<OrbitRuntime>,
    row: orbit_core::application::task::TaskRow,
}

/// Whether the raw query selects the aggregate (`workspace=all`) scope.
fn workspace_is_all(query: Option<&str>) -> bool {
    query.is_some_and(|query| {
        url::form_urlencoded::parse(query.as_bytes())
            .any(|(key, value)| key == "workspace" && value == "all")
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
fn find_task_owner(state: &DashboardState, id: &str) -> Result<TaskOwner, orbit_core::OrbitError> {
    let pinned = state.pin();
    let home = orbit_common::fs::path::home_dir(&orbit_core::composition::composition_env()).ok();
    let mut skipped: Option<orbit_core::OrbitError> = None;
    for entry in pinned
        .entries()
        .iter()
        .filter(|entry| entry.active)
        .filter(|entry| crate::api::workspaces::entry_is_owned(entry))
    {
        let runtime = match pinned.runtime_for(&entry.id) {
            Ok(runtime) => runtime,
            Err(_) => {
                tracing::warn!(workspace = %entry.id, "task owner lookup could not open workspace");
                skipped.get_or_insert_with(|| {
                    orbit_core::OrbitError::Execution(format!(
                        "failed to open workspace '{}'",
                        entry.id
                    ))
                });
                continue;
            }
        };
        match runtime.get_task_row(id) {
            Ok(row) => {
                return Ok(TaskOwner {
                    id: entry.id.clone(),
                    name: entry.name.clone(),
                    root: crate::api::workspaces::abbreviate_home(
                        &entry.repo_root,
                        home.as_deref(),
                    ),
                    runtime,
                    row,
                });
            }
            Err(orbit_core::OrbitError::NotFound {
                kind: orbit_core::NotFoundKind::Task,
                ..
            }) => {}
            Err(error) => {
                tracing::warn!(workspace = %entry.id, %error, "task owner lookup could not read workspace");
                skipped.get_or_insert(error);
            }
        }
    }
    Err(skipped.unwrap_or_else(|| {
        orbit_core::OrbitError::not_found(orbit_core::NotFoundKind::Task, id.to_string())
    }))
}

/// Render `row`'s full projection from `runtime`, stamping the owning
/// workspace's `(id, name, root)` on it when the read resolved through the
/// aggregate.
async fn task_detail_response(
    state: &DashboardState,
    runtime: Arc<OrbitRuntime>,
    row: orbit_core::application::task::TaskRow,
    stamp: Option<(String, String, String)>,
) -> Response {
    let riskiness = cached_riskiness_model(state, &runtime).await;
    match blocking("task detail", move || {
        let statuses = dashboard_status_index(&runtime, &row.task)?;
        let mut value = task_detail_json(
            &runtime,
            &row,
            &statuses,
            riskiness.as_deref(),
            &DashboardPageReads::default(),
        )?;
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
pub(in crate::api) async fn completion_by_complexity(Ws(runtime): Ws) -> Response {
    let runtime_clone = runtime.clone();
    let rows = match helpers::blocking_join("completion-by-complexity aggregation", move || {
        runtime_clone.task_completion_by_complexity()
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
