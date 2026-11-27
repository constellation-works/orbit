//! Task creation, updates, comments and mutation responses.

use super::request::{
    CommentBody, CreateTaskBody, UpdateTaskBody, reject_unsupported_task_body_fields,
};
use crate::api::forward::web_write_caller;
use crate::api::helpers;
use crate::api::{bad_request, map_runtime_error, non_empty_string, server_error, validate_id};
use crate::state::{DashboardState, WsCore};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Json, Response};
use orbit_core::app::task::{self, Command, DashboardAction, DashboardWrite, Outcome};
use orbit_core::application::task::{TaskAddParams, TaskUpdateAuthority, TaskUpdateParams};
use orbit_core::{Core, TaskComplexity};
use orbit_types::identity::{
    agent_family_from_cli, all_agent_families, infer_agent_family_from_model,
};
use serde_json::{Value, json};
use std::sync::Arc;

/// Actor recorded for a dashboard-authored comment when the request supplies no
/// usable human identity. The dashboard is a human-operated surface, so this is
/// the floor — never the server process's ambient agent identity.
const HUMAN_ACTOR_LABEL: &str = "human";

/// Audit and provenance metadata attached to one web task mutation.
pub(in crate::api) struct WebTaskMutationMeta {
    pub(in crate::api) verb: &'static str,
    pub(in crate::api) target_id: Option<String>,
    pub(in crate::api) caller: Option<String>,
    pub(in crate::api) role: Option<String>,
    pub(in crate::api) model: Option<String>,
    pub(in crate::api) detail: Value,
}

impl WebTaskMutationMeta {
    pub(in crate::api) fn new(
        state: &DashboardState,
        headers: &HeaderMap,
        verb: &'static str,
        target_id: Option<String>,
        detail: Value,
    ) -> Self {
        Self {
            verb,
            target_id,
            caller: web_write_caller(state, headers),
            role: None,
            model: None,
            detail,
        }
    }
}

/// Which half of [`task_mutation_response`] failed, so each keeps the status
/// mapping it had when these handlers were written inline: a refused mutation
/// is the caller's problem, a failed render is the server's.
enum TaskMutationFailure {
    Mutation(orbit_core::OrbitError),
    Render(orbit_core::OrbitError),
}

/// Run one dashboard write on the current blocking worker, through the task
/// app. The app applies it with web write provenance active and records the
/// owner-side `task.mutation` audit row.
pub(in crate::api) fn run_audited_web_task_mutation(
    core: &Core,
    meta: WebTaskMutationMeta,
    action: DashboardAction,
) -> Result<Outcome, orbit_core::OrbitError> {
    let WebTaskMutationMeta {
        verb,
        target_id,
        caller,
        role,
        model,
        detail,
    } = meta;
    task::exec(
        core,
        Command::dashboard(DashboardWrite {
            verb,
            target_id,
            caller,
            role,
            model,
            detail,
            action,
        }),
    )
}

/// Apply a task mutation and render its response payload, both on the blocking
/// pool rather than on the tokio worker serving the request. The payload is
/// the task, plus the write's `warnings` and `context_files_unverified`
/// selectors when there are any.
///
/// ORB-10988 / F2026-07-119: every mutation here descends into an exclusive,
/// timeout-free `flock` on the task bundle, and the reads that build the
/// response body hit the store too. Called inline, a burst of dashboard writes
/// parks one async worker per request until the whole pool is blocked and the
/// server stops accepting connections; moved here, the burst queues on the
/// blocking pool and the async workers keep serving.
pub(super) async fn task_mutation_response(
    core: Arc<Core>,
    label: &'static str,
    meta: WebTaskMutationMeta,
    action: DashboardAction,
) -> Response {
    let rendered = helpers::blocking_join(label, move || {
        let written = run_audited_web_task_mutation(&core, meta, action)
            .and_then(Outcome::into_screened)
            .map_err(TaskMutationFailure::Mutation)?;
        let mut value = written.document.map_err(TaskMutationFailure::Render)?;
        // The client reads human-crew identity off the task itself, so a
        // write response keeps it as the list and detail reads do.
        if let Some(object) = value.as_object_mut() {
            orbit_core::app::task::stamp_human_crew(object, written.human_crew.as_ref());
        }
        Ok(value)
    })
    .await;
    match rendered {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(TaskMutationFailure::Mutation(e))) => map_runtime_error(e),
        Ok(Err(TaskMutationFailure::Render(e))) => server_error(e),
        Err(response) => *response,
    }
}

/// `POST /tasks` — create a task in the request's workspace.
///
/// Workspace selection is the [`WsCore`] extractor's: the `?workspace=<id>` query
/// parameter picks the target workspace; omitting it falls back to the
/// server's configured default workspace; an unknown id is a 404 and an
/// inactive (stale-path) one a 400 — never a silent fallback. A stray
/// `workspace` body key is rejected with a 400 (see
/// [`CreateTaskBody::workspace`], ORB-00042).
pub(in crate::api) async fn create_task_action(
    State(state): State<DashboardState>,
    headers: HeaderMap,
    WsCore(core): WsCore,
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
        Err(error) => return bad_request(error.to_string()),
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
        // The dashboard has no operator grant path; `orbit.task.add` and
        // `orbit.task.update` under operator authority are the only writers.
        granted_tools: Vec::new(),
        requires_labels: Vec::new(),
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
        crew_source: None,
        orchestrator: body.orchestrator,
        orchestrator_model: body.orchestrator_model,
        origin: body.origin,
        rule_fingerprint: None,
        context_creation: Default::default(),
        creation_decision: None,
        not_before: None,
    };
    let mut meta = WebTaskMutationMeta::new(
        &state,
        &headers,
        "add",
        None,
        json!({ "status": params.status }),
    );
    meta.model = model;
    task_mutation_response(
        core,
        "task creation",
        meta,
        DashboardAction::Add {
            params: Box::new(params),
            allow_missing_context,
        },
    )
    .await
}

/// `PATCH /tasks/:id` — apply a partial update to a task.
///
/// Every submitted key is applied or refused (ORB-10648): declared fields go
/// into [`TaskUpdateParams`], `model` becomes the write's provenance, and any
/// other key is a 400 from [`reject_unsupported_task_body_fields`]. A caller
/// therefore never receives a `200` for a field this endpoint discarded.
///
/// `force` (ORB-12445) makes this the dashboard's counterpart to `orbit task
/// update --force`: the status change is applied under `StatusAuthority::Forced`
/// and recorded in task history as `forced`. The dashboard is an operator
/// surface, so it carries the same human override the bare CLI does; the agent
/// tool and MCP surfaces still do not declare the field.
pub(in crate::api) async fn update_task_action(
    State(state): State<DashboardState>,
    headers: HeaderMap,
    WsCore(core): WsCore,
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
        Err(error) => return bad_request(error.to_string()),
    };
    if body.force && body.status.is_none() {
        return bad_request(
            "`force` overrides the lifecycle table for a status change, so it requires \
             `status`; send the target status or drop `force`"
                .to_string(),
        );
    }
    let status_note = body.note.as_deref().and_then(non_empty_string);

    let model = body.model.as_deref().and_then(non_empty_string);
    let allow_missing_context = body.allow_missing_context;
    let authority = if body.force {
        TaskUpdateAuthority::Forced
    } else {
        TaskUpdateAuthority::Lifecycle
    };
    let not_before = match body
        .not_before
        .map(|raw| {
            if raw.trim().is_empty() {
                Ok(None)
            } else {
                orbit_types::task::parse_not_before(&raw).map(Some)
            }
        })
        .transpose()
    {
        Ok(value) => value,
        Err(error) => return map_runtime_error(error.into()),
    };
    let params = TaskUpdateParams {
        title: body.title,
        description: body.description,
        acceptance_criteria: body.acceptance_criteria,
        dependencies: body.dependencies,
        relations: body.relations,
        tags: body.tags,
        // No dashboard grant path; see the create handler.
        granted_tools: None,
        requires_labels: None,
        plan: body.plan,
        execution_summary: body.execution_summary,
        comment: body.comment,
        review_settlement: None,
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
        origin: body.origin,
        context_files: body.context_files,
        context_creation: Default::default(),
        upsert_artifacts: Vec::new(),
        trusted_artifact_origin: None,
        discard_candidate: false,
        finding_acceptance: None,
        allow_drop_system_tags: body.allow_drop_system_tags,
        not_before,
    };
    let id = id.to_string();
    let mut meta = WebTaskMutationMeta::new(
        &state,
        &headers,
        "update",
        Some(id.clone()),
        json!({
            "status": params.status,
            "force": body.force,
        }),
    );
    meta.model = model;
    task_mutation_response(
        core,
        "task update",
        meta,
        DashboardAction::Update {
            id,
            params: Box::new(params),
            allow_missing_context,
            authority,
            status_note,
        },
    )
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
pub(in crate::api) async fn add_task_comment_action(
    State(state): State<DashboardState>,
    headers: HeaderMap,
    WsCore(core): WsCore,
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
    let id = id.to_string();
    let mut meta =
        WebTaskMutationMeta::new(&state, &headers, "comment", Some(id.clone()), json!({}));
    meta.role = Some(author.clone());
    task_mutation_response(
        core,
        "task comment",
        meta,
        DashboardAction::Comment {
            id,
            message,
            author,
        },
    )
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
pub(in crate::api) fn human_comment_author(requested: Option<&str>) -> String {
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
