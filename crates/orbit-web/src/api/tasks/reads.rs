//! Task list, detail, locks and completion reads.

use crate::api::pagination::TaskPageQuery;
use crate::api::{bad_request, blocking, server_error, validate_id};
use crate::projections::{TaskListProjection, cached_riskiness_model, task_detail_json};
use crate::state::{DashboardState, Ws};
use axum::extract::{Path, RawQuery, State};
use axum::response::{IntoResponse, Json, Response};
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
///   "offset": 0, "next_cursor": "..." }
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
/// The cross-workspace `/api/tasks/all` aggregate uses the same contract.
pub(in crate::api) async fn list_tasks(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    RawQuery(query): RawQuery,
) -> Response {
    let query = match TaskPageQuery::parse(query.as_deref()) {
        Ok(query) => query,
        Err(message) => return bad_request(message),
    };
    let riskiness = cached_riskiness_model(&state, &runtime).await;
    match blocking("task list", move || {
        let scope = format!("workspace:{}", runtime.workspace_id()?);
        let mut query = query;
        query
            .bind_cursor(&scope)
            .map_err(orbit_core::OrbitError::InvalidInput)?;
        Ok(task_list_page_json(&runtime, &query, &scope, riskiness))
    })
    .await
    {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(e)) => server_error(e),
        Err(response) => *response,
    }
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
    let total = page.total_without_cursor;
    let status_by_id = page.status_by_id;
    let attention = runtime.delivery_attention_map(page.items.iter())?;
    let projection = TaskListProjection::new(runtime)
        .with_delivery_attention(attention)
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
    Ok(json!({
        "truncated": total > items.len(),
        "items": items,
        "total": total,
        "limit": query.limit(),
        "offset": offset,
        "next_cursor": next_cursor,
    }))
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

pub(in crate::api) async fn get_task(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Path(id): Path<String>,
) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id,
        Err(message) => return bad_request(message),
    };
    let id = id.to_string();
    let riskiness = cached_riskiness_model(&state, &runtime).await;
    match blocking("task detail", move || {
        let row = runtime.get_task_row(&id)?;
        Ok(dashboard_status_index(&runtime, &row.task)
            .and_then(|statuses| task_detail_json(&runtime, &row, &statuses, riskiness.as_deref())))
    })
    .await
    {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(e)) => server_error(e),
        Err(response) => *response,
    }
}

/// Status distribution per complexity bucket, including the explicit `unset`
/// band. Counts come from the generated task index — no per-request YAML reads.
pub(in crate::api) async fn completion_by_complexity(Ws(runtime): Ws) -> Response {
    let runtime_clone = runtime.clone();
    let rows =
        match tokio::task::spawn_blocking(move || runtime_clone.task_completion_by_complexity())
            .await
        {
            Ok(Ok(rows)) => rows,
            Ok(Err(e)) => return server_error(e),
            Err(join_err) => {
                return server_error(orbit_core::OrbitError::Execution(format!(
                    "completion-by-complexity aggregation panicked: {join_err}"
                )));
            }
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
