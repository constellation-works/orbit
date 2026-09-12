//! Task CRUD and lifecycle handlers.

use std::sync::Arc;

use crate::state::Ws;
use axum::body::Body;
use axum::extract::{Path, RawQuery};
use axum::http::{HeaderName, HeaderValue, header};
use axum::response::{IntoResponse, Json, Response};
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{
    ExternalRef, OrbitRuntime, Task, TaskComplexity, TaskCreateStatus, TaskPriority, TaskStatus,
    TaskType,
};
use orbit_types::identity::{
    agent_family_from_cli, all_agent_families, infer_agent_family_from_model,
};
use orbit_types::task::TaskRelation;
use orbit_types::task::{inline_safe_artifact_media_type, validate_relative_artifact_path};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value, json};

use super::pagination::TaskPageQuery;
use super::{
    bad_request, blocking, map_runtime_error, non_empty_string, server_error, validate_id,
};
use crate::projections::{task_locks_json, task_row_to_json, task_to_json_with_sidecars};

/// Actor recorded for a dashboard-authored comment when the request supplies no
/// usable human identity. The dashboard is a human-operated surface, so this is
/// the floor — never the server process's ambient agent identity.
const HUMAN_ACTOR_LABEL: &str = "human";

/// Which half of [`task_mutation_response`] failed, so each keeps the status
/// mapping it had when these handlers were written inline: a refused mutation
/// is the caller's problem, a failed render is the server's.
enum TaskMutationFailure {
    Mutation(orbit_core::OrbitError),
    Render(orbit_core::OrbitError),
}

/// Apply a task mutation and render its response payload, both on the blocking
/// pool rather than on the tokio worker serving the request.
///
/// ORB-10988 / F2026-07-119: every mutation here descends into an exclusive,
/// timeout-free `flock` on the task bundle, and the reads that build the
/// response body hit the store too. Called inline, a burst of dashboard writes
/// parks one async worker per request until the whole pool is blocked and the
/// server stops accepting connections; moved here, the burst queues on the
/// blocking pool and the async workers keep serving.
async fn task_mutation_response<F>(
    runtime: Arc<OrbitRuntime>,
    label: &'static str,
    mutate: F,
) -> Response
where
    F: FnOnce(&OrbitRuntime) -> Result<Task, orbit_core::OrbitError> + Send + 'static,
{
    let rendered = tokio::task::spawn_blocking(move || {
        let task = mutate(&runtime).map_err(TaskMutationFailure::Mutation)?;
        let status_by_id = dashboard_status_index(&runtime).map_err(TaskMutationFailure::Render)?;
        task_to_json_with_sidecars(&runtime, &task, &status_by_id)
            .map_err(TaskMutationFailure::Render)
    })
    .await;
    match rendered {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(TaskMutationFailure::Mutation(e))) => map_runtime_error(e),
        Ok(Err(TaskMutationFailure::Render(e))) => server_error(e),
        Err(join_err) => server_error(orbit_core::OrbitError::Execution(format!(
            "{label} panicked: {join_err}"
        ))),
    }
}

struct ArtifactResponsePolicy {
    content_type: &'static str,
    attachment: bool,
}

#[derive(Deserialize, Default)]
pub(super) struct ApproveBody {
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    comment: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct RejectBody {
    note: String,
    #[serde(default)]
    comment: Option<String>,
}

/// Body for `POST /tasks/:id/comments`. `author` is the operator's own name;
/// it is sanitized to a human identity by [`human_comment_author`], never taken
/// as-is when it names an agent family or a model.
#[derive(Deserialize, Default)]
pub(super) struct CommentBody {
    #[serde(default)]
    message: String,
    #[serde(default)]
    author: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct CreateTaskBody {
    title: String,
    description: String,
    #[serde(default)]
    acceptance_criteria: Vec<String>,
    #[serde(default)]
    dependencies: Vec<String>,
    #[serde(default)]
    relations: Vec<TaskRelation>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    required_tools: Vec<String>,
    #[serde(default)]
    plan: String,
    #[serde(default)]
    context_files: Vec<String>,
    /// Escape for a `context_files` selector that names a target this task is
    /// about to create, mirroring `orbit task add --allow-missing-context` and
    /// the `orbit.task.add` tool's `allow_missing_context` input. See
    /// [`OrbitRuntime::ensure_context_selectors_exist`](orbit_core::OrbitRuntime::ensure_context_selectors_exist).
    #[serde(default)]
    allow_missing_context: bool,
    #[serde(default)]
    external_refs: Vec<ExternalRef>,
    /// Trap field (ORB-00042): `workspace` is a *workspace selector* and this
    /// endpoint takes it as the `?workspace=<id>` query parameter, never as a
    /// body field. Historically bridge sent `{"workspace": <path>}` here and
    /// serde silently dropped the unknown key, so every task landed in the
    /// default workspace. Deserializing the key just to reject it makes the
    /// mistake a loud 400. A `#[serde(deny_unknown_fields)]` would break every
    /// tolerant caller that sends extra keys.
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default = "default_priority")]
    priority: TaskPriority,
    complexity: TaskComplexity,
    #[serde(default)]
    task_type: Option<TaskType>,
    #[serde(default)]
    status: Option<TaskCreateStatus>,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    source_task_id: Option<String>,
    #[serde(default)]
    crew: Option<String>,
    #[serde(default)]
    orchestrator: Option<String>,
    /// Caller-supplied provenance, forwarded as the write's model identity the
    /// same way `POST /api/frictions` takes it. Before
    /// ORB-10648 this key was undeclared, so serde dropped it and the task was
    /// attributed to the ambient identity while the caller was told `model` had
    /// been applied.
    #[serde(default)]
    model: Option<String>,
    /// Retired create input, declared so it stays *knowingly* tolerated rather
    /// than falling into [`CreateTaskBody::unsupported`]. `comment` is one of
    /// [`RETIRED_TASK_ADD_INPUT_FIELDS`](orbit_common::protocol::tool_input::RETIRED_TASK_ADD_INPUT_FIELDS):
    /// the native `orbit.task.add` tool now rejects it with `invalid_input`.
    /// This HTTP body still accepts the key so existing dashboard clients are
    /// not broken; comment on a task with `POST /tasks/:id/comments`.
    #[serde(default)]
    comment: Option<String>,
    /// Trap field (ORB-10648): attribution was consolidated to `model`-only, so
    /// an `agent` key is a caller bug rather than a usable input. The native
    /// tool surface rejects it outright (`reject_agent_field`); this body does
    /// the same instead of dropping it.
    #[serde(default)]
    agent: Option<String>,
    /// Every key this endpoint does not declare, captured rather than dropped.
    /// See [`reject_unsupported_task_body_fields`].
    #[serde(flatten)]
    unsupported: Map<String, Value>,
}

fn default_priority() -> TaskPriority {
    TaskPriority::Medium
}

/// Reject a task body carrying keys the endpoint would otherwise discard.
///
/// ORB-10648: both task bodies derived a plain `Deserialize`, so any undeclared
/// key (`priority` on update, an `agent` typo, a field only the native
/// `orbit.task.update` tool declares) was silently dropped and the handler
/// still answered `200` with the task JSON. A caller that reports per-field
/// application — bridge's `orbit_task_update` write confirmation — then
/// affirmatively reports a field as applied that was never persisted, and a
/// false "applied" is worse than an error because nothing prompts a read-back.
///
/// The contract is all-or-nothing: an unsupported key fails the whole request,
/// so no write lands partially. The keys are captured through
/// `#[serde(flatten)]` rather than `#[serde(deny_unknown_fields)]` for the same
/// reason [`CreateTaskBody::workspace`] is a declared trap field (ORB-00042):
/// the diagnostic stays Orbit's own, naming every offending key and pointing at
/// the surface that does accept it, instead of serde's opaque message.
fn reject_unsupported_task_body_fields(
    agent: Option<&String>,
    unsupported: &Map<String, Value>,
    endpoint: &str,
) -> Option<Response> {
    if agent.is_some() {
        return Some(bad_request(format!(
            "{endpoint} no longer accepts `agent`; use `model` with the agent \
             family (codex, claude, gemini, or grok) for attribution"
        )));
    }
    if unsupported.is_empty() {
        return None;
    }
    let mut names = unsupported.keys().cloned().collect::<Vec<_>>();
    names.sort();
    let names = names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(bad_request(format!(
        "unsupported body field(s) {names}: {endpoint} does not apply them, so the \
         request is rejected rather than reporting a write that would not land"
    )))
}

/// Partial-update body for `PATCH /tasks/:id`. Each field is `Option<...>`;
/// fields absent from the JSON body remain unchanged.
///
/// Every key the caller sends is either applied or refused: unknown keys land
/// in [`UpdateTaskBody::unsupported`] and fail the request
/// ([`reject_unsupported_task_body_fields`], ORB-10648). Nothing is dropped in
/// silence.
#[derive(Deserialize, Default)]
pub(super) struct UpdateTaskBody {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    acceptance_criteria: Option<Vec<String>>,
    #[serde(default)]
    dependencies: Option<Vec<String>>,
    #[serde(default)]
    relations: Option<Vec<TaskRelation>>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    plan: Option<String>,
    #[serde(default)]
    execution_summary: Option<String>,
    #[serde(default)]
    comment: Option<String>,
    #[serde(default)]
    status: Option<TaskStatus>,
    /// Replacement dispatch priority (ORB-10648). Previously undeclared here
    /// even though the record layer could persist it, so an operator's
    /// re-prioritization was dropped while the response reported success.
    #[serde(default)]
    priority: Option<TaskPriority>,
    #[serde(default)]
    complexity: Option<TaskComplexity>,
    #[serde(default)]
    task_type: Option<TaskType>,
    #[serde(default, deserialize_with = "deserialize_nullable_string_patch_field")]
    pr_status: Option<Option<String>>,
    #[serde(default)]
    context_files: Option<Vec<String>>,
    /// Escape for a `context_files` selector that names a target this task is
    /// about to create. See [`CreateTaskBody::allow_missing_context`].
    #[serde(default)]
    allow_missing_context: bool,
    #[serde(default, deserialize_with = "deserialize_nullable_string_patch_field")]
    crew: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_nullable_string_patch_field")]
    orchestrator: Option<Option<String>>,
    /// Caller-supplied provenance, forwarded as the write's model identity.
    /// See [`CreateTaskBody::model`].
    #[serde(default)]
    model: Option<String>,
    /// Trap field. See [`CreateTaskBody::agent`].
    #[serde(default)]
    agent: Option<String>,
    /// Every key this endpoint does not declare, captured rather than dropped.
    /// See [`reject_unsupported_task_body_fields`].
    #[serde(flatten)]
    unsupported: Map<String, Value>,
}

fn deserialize_nullable_string_patch_field<'de, D>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
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
/// { "items": [ /* task objects, newest first */ ],
///   "total": 137, "limit": 50, "truncated": true,
///   "offset": 0, "next_cursor": "..." }
/// ```
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
pub(super) async fn list_tasks(Ws(runtime): Ws, RawQuery(query): RawQuery) -> Response {
    let mut query = match TaskPageQuery::parse(query.as_deref()) {
        Ok(query) => query,
        Err(message) => return bad_request(message),
    };
    let scope = match runtime.workspace_id() {
        Ok(workspace_id) => format!("workspace:{workspace_id}"),
        Err(error) => return server_error(error),
    };
    if let Err(message) = query.bind_cursor(&scope) {
        return bad_request(message);
    }
    match blocking("task list", move || {
        Ok(task_list_page_json(&runtime, &query, &scope))
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
) -> Result<Value, orbit_core::OrbitError> {
    let total = runtime.task_candidates(&query.count_filter(), 0)?.total;
    let page = runtime.query_task_rows(&orbit_core::application::task::TaskListQuery {
        filter: query.filter(),
        limit: query.limit(),
        ..Default::default()
    })?;
    let status_by_id = page.status_by_id;
    let items = page
        .items
        .iter()
        .map(|row| task_row_to_json(runtime, row, &status_by_id))
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

pub(super) async fn list_task_locks(Ws(runtime): Ws) -> Response {
    match blocking("task locks", move || Ok(task_locks_json(&runtime))).await {
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
fn dashboard_status_index(
    runtime: &OrbitRuntime,
) -> Result<std::collections::BTreeMap<String, TaskStatus>, orbit_core::OrbitError> {
    runtime.task_status_index()
}

pub(super) async fn get_task(Ws(runtime): Ws, Path(id): Path<String>) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id,
        Err(message) => return bad_request(message),
    };
    let id = id.to_string();
    match blocking("task detail", move || {
        let row = runtime.get_task_row(&id)?;
        Ok(dashboard_status_index(&runtime)
            .and_then(|statuses| task_row_to_json(&runtime, &row, &statuses)))
    })
    .await
    {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(e)) => server_error(e),
        Err(response) => *response,
    }
}

pub(super) async fn get_task_artifact(
    Ws(runtime): Ws,
    Path((id, path)): Path<(String, String)>,
) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id,
        Err(message) => return bad_request(message),
    };
    let path = match validate_artifact_request_path(&path) {
        Ok(path) => path,
        Err(message) => return bad_request(message),
    };
    let id = id.to_string();
    let path_owned = path.to_string();
    let missing = format!("artifact not found: {id}/{path_owned}");
    match blocking("task artifact", move || {
        runtime.get_task_artifact(&id, &path_owned)
    })
    .await
    {
        Ok(Some(artifact)) => {
            let policy = artifact_response_policy(&artifact.media_type);
            let mut response = Response::new(Body::from(artifact.content));
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static(policy.content_type),
            );
            response.headers_mut().insert(
                HeaderName::from_static("x-content-type-options"),
                HeaderValue::from_static("nosniff"),
            );
            if policy.attachment {
                response.headers_mut().insert(
                    header::CONTENT_DISPOSITION,
                    HeaderValue::from_static("attachment"),
                );
            }
            response
        }
        Ok(None) => super::not_found(missing),
        Err(response) => *response,
    }
}

fn artifact_response_policy(media_type: &str) -> ArtifactResponsePolicy {
    // The inline allowlist is the shared artifact policy, not a web-only rule:
    // the dashboard and `orbit.task.artifact.get` must agree on which media
    // types may be rendered rather than downloaded.
    let content_type = inline_safe_artifact_media_type(media_type);
    ArtifactResponsePolicy {
        content_type: content_type.unwrap_or("application/octet-stream"),
        attachment: content_type.is_none(),
    }
}

fn validate_artifact_request_path(path: &str) -> Result<String, String> {
    validate_relative_artifact_path(path).map_err(|error| error.to_string())?;
    Ok(path.to_string())
}

/// `POST /tasks` — create a task in the request's workspace.
///
/// Workspace selection is the [`Ws`] extractor's: the `?workspace=<id>` query
/// parameter picks the target workspace; omitting it falls back to the
/// server's configured default workspace; an unknown id is a 404 and an
/// inactive (stale-path) one a 400 — never a silent fallback. A stray
/// `workspace` body key is rejected with a 400 (see
/// [`CreateTaskBody::workspace`], ORB-00042).
pub(super) async fn create_task_action(
    Ws(runtime): Ws,
    Json(body): Json<CreateTaskBody>,
) -> Response {
    if body.workspace.is_some() {
        return bad_request(
            "unsupported body field `workspace`: select the target workspace with the \
             `?workspace=<id>` query parameter"
                .to_string(),
        );
    }
    if let Some(response) = reject_unsupported_task_body_fields(
        body.agent.as_ref(),
        &body.unsupported,
        "POST /api/tasks",
    ) {
        return response;
    }
    if body.comment.is_some() {
        tracing::warn!(
            target: "orbit.dashboard.tasks",
            field = "comment",
            "ignored retired POST /api/tasks field; comment with POST /api/tasks/:id/comments"
        );
    }
    let complexity = match body.complexity.require_assessed() {
        Ok(complexity) => complexity,
        Err(message) => return bad_request(message),
    };
    let model = body.model.as_deref().and_then(non_empty_string);
    let allow_missing_context = body.allow_missing_context;
    let params = TaskAddParams {
        parent_id: body.parent_id,
        title: body.title,
        description: body.description,
        acceptance_criteria: body.acceptance_criteria,
        dependencies: body.dependencies,
        relations: body.relations,
        tags: body.tags,
        required_tools: body.required_tools,
        plan: body.plan,
        comment: None,
        context_files: body.context_files,
        priority: body.priority,
        complexity,
        task_type: body.task_type,
        status: body.status.map(Into::into),
        system_created: false,
        external_refs: body.external_refs,
        source_task_id: body.source_task_id,
        crew: body.crew,
        orchestrator: body.orchestrator,
    };
    task_mutation_response(runtime, "task creation", move |runtime| {
        if !allow_missing_context {
            runtime.ensure_context_selectors_exist(&params.context_files)?;
        }
        runtime.add_task_with_identity(params, None, model)
    })
    .await
}

/// `PATCH /tasks/:id` — apply a partial update to a task.
///
/// Every submitted key is applied or refused (ORB-10648): declared fields go
/// into [`TaskUpdateParams`], `model` becomes the write's provenance, and any
/// other key is a 400 from [`reject_unsupported_task_body_fields`]. A caller
/// therefore never receives a `200` for a field this endpoint discarded.
pub(super) async fn update_task_action(
    Ws(runtime): Ws,
    Path(id): Path<String>,
    Json(body): Json<UpdateTaskBody>,
) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id,
        Err(message) => return bad_request(message),
    };
    if let Some(response) = reject_unsupported_task_body_fields(
        body.agent.as_ref(),
        &body.unsupported,
        "PATCH /api/tasks/:id",
    ) {
        return response;
    }
    let complexity = match body
        .complexity
        .map(TaskComplexity::require_assessed)
        .transpose()
    {
        Ok(complexity) => complexity,
        Err(message) => return bad_request(message),
    };
    let model = body.model.as_deref().and_then(non_empty_string);
    let allow_missing_context = body.allow_missing_context;
    let params = TaskUpdateParams {
        title: body.title,
        description: body.description,
        acceptance_criteria: body.acceptance_criteria,
        dependencies: body.dependencies,
        relations: body.relations,
        tags: body.tags,
        plan: body.plan,
        execution_summary: body.execution_summary,
        comment: body.comment,
        status: body.status,
        priority: body.priority,
        complexity,
        task_type: body.task_type,
        source_task_id: None,
        planned_by: None,
        implemented_by: None,
        pr_status: body.pr_status,
        job_run_id: None,
        crew: body.crew,
        orchestrator: body.orchestrator,
        context_files: body.context_files,
        upsert_artifacts: Vec::new(),
    };
    let id = id.to_string();
    task_mutation_response(runtime, "task update", move |runtime| {
        if !allow_missing_context && let Some(candidates) = params.context_files.as_deref() {
            runtime.ensure_context_selectors_exist(candidates)?;
        }
        runtime.update_task_with_identity(&id, params, None, model)
    })
    .await
}

/// `POST /tasks/:id/comments` — append a human comment to a task.
///
/// Comments are stored in the task's existing review-thread structure (the
/// bundle's `comments.jsonl`, written through `TaskUpdateParams::comment`), so
/// this adds no parallel persistence model on the task record.
///
/// Authorship is forced to a human identity (ORB-10444). The dashboard server
/// process may itself be running inside a managed Orbit run, where the runtime's
/// ambient actor is an agent model — attributing an operator's note to that
/// model would be a lie, so the author comes from the request (sanitized by
/// [`human_comment_author`]) and never from the ambient identity.
pub(super) async fn add_task_comment_action(
    Ws(runtime): Ws,
    Path(id): Path<String>,
    Json(body): Json<CommentBody>,
) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id,
        Err(message) => return bad_request(message),
    };
    let Some(message) = non_empty_string(&body.message) else {
        return bad_request("comment message must not be empty".to_string());
    };
    let author = human_comment_author(body.author.as_deref());
    let params = TaskUpdateParams {
        comment: Some(message),
        ..TaskUpdateParams::default()
    };
    let id = id.to_string();
    task_mutation_response(runtime, "task comment", move |runtime| {
        runtime.update_task_with_identity(&id, params, Some(author), None)
    })
    .await
}

/// Resolve the author label recorded for a dashboard comment.
///
/// A caller-supplied label is kept only if it is a genuine human identity: a
/// blank value, a known agent family (`codex`/`claude`/…), a string that maps to
/// one of those families as a model constant, or the `system`/`agent` role words
/// all collapse to [`HUMAN_ACTOR_LABEL`]. That keeps a model constant out of the
/// `by` field whether it arrives from a confused client or from the ambient
/// identity the runtime would otherwise supply.
fn human_comment_author(requested: Option<&str>) -> String {
    let Some(label) = requested.and_then(non_empty_string) else {
        return HUMAN_ACTOR_LABEL.to_string();
    };
    let normalized = agent_family_from_cli(&label);
    let looks_like_agent = normalized == "system"
        || normalized == "agent"
        || all_agent_families().contains(&normalized.as_str())
        || infer_agent_family_from_model(&label).is_some();
    if looks_like_agent {
        HUMAN_ACTOR_LABEL.to_string()
    } else {
        label
    }
}

pub(super) async fn approve_task_action(
    Ws(runtime): Ws,
    Path(id): Path<String>,
    body: Option<Json<ApproveBody>>,
) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id,
        Err(message) => return bad_request(message),
    };
    let body = body.map(|Json(b)| b).unwrap_or_default();
    let id = id.to_string();
    task_mutation_response(runtime, "task approval", move |runtime| {
        runtime.approve_task(&id, body.note, body.comment)
    })
    .await
}

pub(super) async fn reject_task_action(
    Ws(runtime): Ws,
    Path(id): Path<String>,
    Json(body): Json<RejectBody>,
) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id,
        Err(message) => return bad_request(message),
    };
    let id = id.to_string();
    task_mutation_response(runtime, "task rejection", move |runtime| {
        runtime.reject_task(&id, body.note, body.comment)
    })
    .await
}

pub(super) async fn archive_task_action(Ws(runtime): Ws, Path(id): Path<String>) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id,
        Err(message) => return bad_request(message),
    };
    let archived_id = id.to_string();
    let runtime_clone = runtime.clone();
    match blocking("task archival", move || {
        runtime_clone.archive_task(&archived_id)
    })
    .await
    {
        Ok(()) => Json(json!({ "ok": true, "id": id })).into_response(),
        Err(response) => *response,
    }
}

/// Status distribution per complexity bucket, including the explicit `unset`
/// band. Counts come from the generated task index — no per-request YAML reads.
pub(super) async fn completion_by_complexity(Ws(runtime): Ws) -> Response {
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
