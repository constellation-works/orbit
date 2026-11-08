//! The Automation › Residents view: set up and manage resident mandates.
//!
//! Reads and writes go through the `orbit.resident.*` tools, so the view
//! reports and changes exactly what `orbit resident list|show|relay|control`
//! do. Two writes go to core use cases instead. Creation needs a file for
//! `orbit.task.artifact.put`; its use case judges the draft with the same
//! rule every mandate read applies before it writes the tagged assignment
//! and its `resident/mandate.json`. The grant form goes to the dashboard
//! grant operation beside `orbit.resident.grant`'s, which applies the same
//! grant rules.
//!
//! * `GET /api/residents` — every mandate as `orbit.resident.show` reports
//!   it, the configured crews for the form, the workspace's resident cycle
//!   routine and what this session may do.
//! * `GET /api/residents/<id>` — one mandate.
//! * `POST /api/residents` — create a mandate.
//! * `POST /api/residents/<id>/grant` — narrow, or widen, through the core
//!   dashboard grant operation, which owns the widen-then-narrow split of a
//!   mixed form, the answered `resident_authority` request (`via:
//!   decision:<id>`), the `quote` requirement and the outcome vocabulary.
//!   A partial outcome (`granted_narrowing_refused`,
//!   `unchanged_narrowing_refused`) audits as a failure naming which step
//!   applied.
//! * `POST /api/residents/<id>/control` and `/relay` — the manager channel.
//!
//! Every write needs the operator session ([`DASHBOARD_RESIDENT_WRITE`]) and
//! leaves a `dashboard.operations` audit row beside the history entry core
//! writes. Turning the cycle routine on or off is the audited
//! `POST /api/routines/toggle`.

use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Json, Response};
use chrono::Utc;
use orbit_cmd::registry_routines::routine_statuses;
use orbit_common::governance::authorization::{DASHBOARD_RESIDENT_WRITE, DASHBOARD_ROUTINE_TOGGLE};
use orbit_core::application::resident::{
    ResidentDashboardGrant, ResidentDashboardGrantOutcome, ResidentDashboardGrantParams,
    ResidentMandateCreateParams,
};
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_types::task::TaskComplexity;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::routines::{
    action_capability, authorization_denied, authorized_caller, record_operation_audit, status_json,
};
use super::tasks::human_comment_author;
use super::{blocking, workspace_label};
use crate::state::{DashboardState, Ws};

/// The job every workspace's seeded resident cycle routine targets.
const RESIDENT_CYCLE_TARGET: &str = "job:resident_cycle_pipeline";

/// Query string for `GET /api/residents`.
#[derive(Debug, Default, Deserialize)]
pub(super) struct ResidentListQuery {
    /// Include ended mandates, as `orbit resident list --all` does.
    #[serde(default)]
    all: bool,
}

/// Body of `POST /api/residents`.
#[derive(Debug, Default, Deserialize)]
pub(super) struct ResidentCreateBody {
    /// Assignment title; a shortened goal when blank.
    #[serde(default)]
    title: Option<String>,
    /// Assignment narrative.
    #[serde(default)]
    description: Option<String>,
    /// Assignment complexity; `medium` when absent.
    #[serde(default)]
    complexity: Option<String>,
    /// The `resident.mandate/v1` draft.
    #[serde(default)]
    mandate: Option<Value>,
}

/// Body of `POST /api/residents/<id>/grant`: the tool's own fields.
#[derive(Debug, Default, Deserialize)]
pub(super) struct ResidentGrantBody {
    #[serde(default)]
    add: Vec<String>,
    #[serde(default)]
    workspaces: Vec<String>,
    #[serde(default)]
    remove: Vec<String>,
    #[serde(default)]
    drop_workspaces: Vec<String>,
    #[serde(default)]
    deadline: Option<String>,
    /// The operator's words behind a widening, recorded verbatim.
    #[serde(default)]
    quote: Option<String>,
}

/// Body of `POST /api/residents/<id>/control`.
#[derive(Debug, Default, Deserialize)]
pub(super) struct ResidentControlBody {
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    status_request: bool,
    #[serde(default)]
    by_session: Option<String>,
}

/// Body of `POST /api/residents/<id>/relay`.
#[derive(Debug, Default, Deserialize)]
pub(super) struct ResidentRelayBody {
    #[serde(default)]
    quote: Option<String>,
    /// Who said the words; the dashboard operator when blank.
    #[serde(default)]
    human: Option<String>,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    reverses: Option<String>,
}

/// The session the dashboard calls resident tools with. It carries the
/// operator capability only when this server holds an operator session,
/// and never the hosting process's terminal or environment.
fn dashboard_session(operator: bool) -> ToolSessionContext {
    let mut session = ToolSessionContext::default();
    session.effective_capabilities.insert(McpCapability::Agent);
    if operator {
        session
            .effective_capabilities
            .insert(McpCapability::Operator);
    }
    session
}

fn non_blank(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// The workspace's resident cycle routine row, as Routines lists it.
fn resident_cycle_routine(state: &DashboardState, runtime: &OrbitRuntime) -> Value {
    let Ok(report) = routine_statuses(state.global_root()) else {
        return Value::Null;
    };
    // The routine's source is the registered workspace whose `.orbit`
    // directory it was loaded from: this runtime's.
    let canonical = |path: &std::path::Path| std::fs::canonicalize(path).ok();
    let orbit_dir = canonical(&runtime.paths().orbit_dir);
    report
        .statuses
        .iter()
        .find(|status| {
            orbit_dir.is_some()
                && canonical(&status.routine.source_orbit_dir) == orbit_dir
                && status.routine.definition.target.as_ref_string() == RESIDENT_CYCLE_TARGET
        })
        .map_or(Value::Null, |status| {
            let mut row = status_json(status);
            row["machine_name"] = json!(report.machine_name);
            row
        })
}

/// `GET /api/residents`.
pub(super) async fn list_residents(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Query(query): Query<ResidentListQuery>,
) -> Response {
    let operator = state.operator_session();
    let include_ended = query.all;
    match blocking("resident list", move || {
        let session = dashboard_session(operator);
        let listed = runtime.run_tool_with_session(
            "orbit.resident.list",
            json!({"include_ended": include_ended}),
            session.clone(),
        )?;
        let mut mandates = Vec::new();
        for row in listed["mandates"].as_array().into_iter().flatten() {
            let Some(task_id) = row["task_id"].as_str() else {
                continue;
            };
            let mut shown = runtime.run_tool_with_session(
                "orbit.resident.show",
                json!({"task_id": task_id}),
                session.clone(),
            )?;
            shown["goal"] = row["goal"].clone();
            mandates.push(shown);
        }
        let crews = serde_json::to_value(runtime.configured_crew_registry_projection())
            .unwrap_or(Value::Null);
        let crew_names: Vec<Value> = crews["crews"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|crew| crew.get("name").cloned())
            .collect();
        Ok(json!({
            "workspace": runtime.workspace_label(),
            "count": mandates.len(),
            "mandates": mandates,
            "crews": crew_names,
            "default_crew": crews["default_crew"],
            "routine": resident_cycle_routine(&state, &runtime),
            "capabilities": {
                "resident_write": action_capability(&DASHBOARD_RESIDENT_WRITE, operator),
                "routine_toggle": action_capability(&DASHBOARD_ROUTINE_TOGGLE, operator),
            },
        }))
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

/// `GET /api/residents/<id>`.
pub(super) async fn show_resident(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Path(id): Path<String>,
) -> Response {
    let operator = state.operator_session();
    match blocking("resident show", move || {
        runtime.run_tool_with_session(
            "orbit.resident.show",
            json!({"task_id": id}),
            dashboard_session(operator),
        )
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

/// Authorize one resident write, audit it, run it and audit the outcome.
/// `partial_failure` names a successful write's outcome the audit row
/// records as a failure.
async fn resident_write<T: Serialize + Send + 'static>(
    state: &DashboardState,
    runtime: Arc<OrbitRuntime>,
    operation: &'static str,
    target: &str,
    arguments: Value,
    write: impl FnOnce(Arc<OrbitRuntime>) -> Result<T, OrbitError> + Send + 'static,
    partial_failure: fn(&T) -> Option<String>,
) -> Response {
    let started = Instant::now();
    let workspace = workspace_label(&runtime).await;
    let caller = match authorized_caller(&DASHBOARD_RESIDENT_WRITE, state.operator_session()) {
        Ok(caller) => caller,
        Err(denial) => {
            record_operation_audit(
                &runtime,
                &workspace,
                operation,
                target,
                "",
                &arguments,
                None,
                Some(&denial),
                None,
                started,
            )
            .await;
            return authorization_denied(denial);
        }
    };
    let writer = Arc::clone(&runtime);
    let result = blocking(operation, move || write(writer)).await;
    let failure: Option<String> = match &result {
        Err(_) => Some("refused".to_string()),
        Ok(value) => partial_failure(value),
    };
    record_operation_audit(
        &runtime,
        &workspace,
        operation,
        target,
        "",
        &arguments,
        Some(&caller),
        None,
        failure.as_deref(),
        started,
    )
    .await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

/// A write whose every success audits as a success.
fn no_partial_failure(_: &Value) -> Option<String> {
    None
}

/// `POST /api/residents` — create a mandate's assignment and artifact.
pub(super) async fn create_resident(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Json(body): Json<ResidentCreateBody>,
) -> Response {
    let mandate = body.mandate.unwrap_or(Value::Null);
    let goal = mandate["goal"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    let title = non_blank(body.title.as_deref()).unwrap_or_else(|| {
        const GOAL_TITLE_CHARS: usize = 60;
        let shortened: String = goal.chars().take(GOAL_TITLE_CHARS).collect();
        let suffix = if goal.chars().count() > GOAL_TITLE_CHARS {
            "…"
        } else {
            ""
        };
        format!("[resident]: {}{suffix}", shortened.trim_end())
    });
    let arguments = json!({"title": title, "mandate": mandate});
    let complexity = body.complexity;
    let description = body.description.unwrap_or_default();
    resident_write(
        &state,
        runtime,
        "resident.create",
        "new",
        arguments,
        move |runtime| {
            if !mandate.is_object() {
                return Err(OrbitError::InvalidInput(
                    "mandate must be a resident.mandate/v1 object".to_string(),
                ));
            }
            let complexity = non_blank(complexity.as_deref())
                .unwrap_or_else(|| "medium".to_string())
                .parse::<TaskComplexity>()
                .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
            runtime.create_resident_mandate(ResidentMandateCreateParams {
                title,
                description,
                complexity,
                mandate,
            })
        },
        no_partial_failure,
    )
    .await
}

/// `POST /api/residents/<id>/grant` — narrow, widen as an answered
/// request, or both, through
/// [`OrbitRuntime::grant_resident_authority_as_dashboard_operator`]. The
/// handler parses the form, authorizes and audits it and renders the core
/// result; a partial outcome audits as a failure naming which step applied.
pub(super) async fn grant_resident(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Path(id): Path<String>,
    Json(body): Json<ResidentGrantBody>,
) -> Response {
    let params = ResidentDashboardGrantParams {
        task_id: id.clone(),
        add: body.add,
        workspaces: body.workspaces,
        remove: body.remove,
        drop_workspaces: body.drop_workspaces,
        deadline: body.deadline,
        quote: body.quote,
        answered_by: human_comment_author(None),
    };
    let mut arguments = Map::new();
    arguments.insert("task_id".into(), json!(id));
    for (key, list) in [
        ("add", &params.add),
        ("workspaces", &params.workspaces),
        ("remove", &params.remove),
        ("drop_workspaces", &params.drop_workspaces),
    ] {
        if !list.is_empty() {
            arguments.insert(key.into(), json!(list));
        }
    }
    for (key, value) in [("deadline", &params.deadline), ("quote", &params.quote)] {
        if let Some(value) = non_blank(value.as_deref()) {
            arguments.insert(key.into(), json!(value));
        }
    }
    let operation = if params.widens() {
        "resident.grant"
    } else {
        "resident.narrow"
    };
    resident_write(
        &state,
        runtime,
        operation,
        &id,
        Value::Object(arguments),
        move |runtime| runtime.grant_resident_authority_as_dashboard_operator(params),
        grant_partial_failure,
    )
    .await
}

/// The audit failure of a grant whose widening outcome stands while its
/// narrowing was refused.
fn grant_partial_failure(grant: &ResidentDashboardGrant) -> Option<String> {
    let error = grant.narrowing_error.as_deref().unwrap_or_default();
    match grant.outcome {
        ResidentDashboardGrantOutcome::GrantedNarrowingRefused => {
            Some(format!("Widening applied; narrowing refused: {error}"))
        }
        ResidentDashboardGrantOutcome::UnchangedNarrowingRefused => {
            Some(format!("No widening applied; narrowing refused: {error}"))
        }
        ResidentDashboardGrantOutcome::Granted
        | ResidentDashboardGrantOutcome::Unchanged
        | ResidentDashboardGrantOutcome::Narrowed
        | ResidentDashboardGrantOutcome::GrantedAndNarrowed
        | ResidentDashboardGrantOutcome::UnchangedAndNarrowed => None,
    }
}

/// `POST /api/residents/<id>/control`.
pub(super) async fn control_resident(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Path(id): Path<String>,
    Json(body): Json<ResidentControlBody>,
) -> Response {
    let mut input = json!({
        "task_id": id,
        "action": body.action.unwrap_or_default(),
        "reason": body.reason.unwrap_or_default(),
    });
    if body.status_request {
        input["status_request"] = json!(true);
    }
    if let Some(session) = non_blank(body.by_session.as_deref()) {
        input["by_session"] = json!(session);
    }
    let target = id.clone();
    resident_write(
        &state,
        runtime,
        "resident.control",
        &target,
        input.clone(),
        move |runtime| {
            runtime.run_tool_with_session("orbit.resident.control", input, dashboard_session(true))
        },
        no_partial_failure,
    )
    .await
}

/// `POST /api/residents/<id>/relay` — the operator's own words, said here.
pub(super) async fn relay_resident(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Path(id): Path<String>,
    Json(body): Json<ResidentRelayBody>,
) -> Response {
    let human = non_blank(body.human.as_deref()).unwrap_or_else(|| human_comment_author(None));
    let mut input = json!({
        "task_id": id,
        "quote": body.quote.unwrap_or_default(),
        "source": {"human": human, "via": "dashboard", "said_at": Utc::now().to_rfc3339()},
    });
    if let Some(request_id) = non_blank(body.request_id.as_deref()) {
        input["request_id"] = json!(request_id);
    }
    if let Some(reverses) = non_blank(body.reverses.as_deref()) {
        input["reverses"] = json!(reverses);
    }
    let target = id.clone();
    resident_write(
        &state,
        runtime,
        "resident.relay",
        &target,
        input.clone(),
        move |runtime| {
            runtime.run_tool_with_session("orbit.resident.relay", input, dashboard_session(true))
        },
        no_partial_failure,
    )
    .await
}
