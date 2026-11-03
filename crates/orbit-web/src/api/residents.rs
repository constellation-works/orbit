//! The Automation › Residents view: set up and manage resident mandates.
//!
//! Reads and writes go through the `orbit.resident.*` tools, so the view
//! reports and changes exactly what `orbit resident list|show|grant|relay|
//! control` do. The one exception is creation, which needs a file for
//! `orbit.task.artifact.put`; it goes to the core use case that judges the
//! draft with the same rule every mandate read applies before it writes the
//! tagged assignment and its `resident/mandate.json`.
//!
//! * `GET /api/residents` — every mandate as `orbit.resident.show` reports
//!   it, the configured crews for the form, the workspace's resident cycle
//!   routine and what this session may do.
//! * `GET /api/residents/<id>` — one mandate.
//! * `POST /api/residents` — create a mandate.
//! * `POST /api/residents/<id>/grant` — narrow, or widen. A dashboard
//!   session is never the native-terminal operator: a widening raises the
//!   `resident_authority` request with the operator's words and the operator
//!   answers it `grant` in the same action, so the change is recorded as
//!   `via: decision:<id>` like any human answer.
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
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_core::application::resident::ResidentMandateCreateParams;
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_types::task::TaskComplexity;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde::Deserialize;
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
    /// Assignment title; the goal when blank.
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
async fn resident_write(
    state: &DashboardState,
    runtime: Arc<OrbitRuntime>,
    operation: &'static str,
    target: &str,
    arguments: Value,
    write: impl FnOnce(Arc<OrbitRuntime>) -> Result<Value, OrbitError> + Send + 'static,
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
    let failure = result.as_ref().err().map(|_| "refused");
    record_operation_audit(
        &runtime,
        &workspace,
        operation,
        target,
        "",
        &arguments,
        Some(&caller),
        None,
        failure,
        started,
    )
    .await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
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
    let title = non_blank(body.title.as_deref()).unwrap_or(goal);
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
    )
    .await
}

/// `POST /api/residents/<id>/grant` — narrow, widen as an answered
/// request, or both as two recorded changes. Core refuses one call that
/// both widens and narrows, so a mixed dashboard call widens first and
/// then narrows; the dashboard form also keeps them separate.
pub(super) async fn grant_resident(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Path(id): Path<String>,
    Json(body): Json<ResidentGrantBody>,
) -> Response {
    let widening = !body.add.is_empty() || !body.workspaces.is_empty();
    let narrowing = !body.remove.is_empty()
        || !body.drop_workspaces.is_empty()
        || non_blank(body.deadline.as_deref()).is_some();
    let quote = non_blank(body.quote.as_deref());
    let mut input = Map::new();
    input.insert("task_id".into(), json!(id));
    for (key, list) in [
        ("add", &body.add),
        ("workspaces", &body.workspaces),
        ("remove", &body.remove),
        ("drop_workspaces", &body.drop_workspaces),
    ] {
        if !list.is_empty() {
            input.insert(key.into(), json!(list));
        }
    }
    if let Some(deadline) = non_blank(body.deadline.as_deref()) {
        input.insert("deadline".into(), json!(deadline));
    }
    if let Some(quote) = &quote {
        input.insert("quote".into(), json!(quote));
    }
    let input = Value::Object(input);
    let operation = if widening {
        "resident.grant"
    } else {
        "resident.narrow"
    };
    let target = id.clone();
    resident_write(
        &state,
        runtime,
        operation,
        &target,
        input.clone(),
        move |runtime| {
            let mut session = dashboard_session(true);
            if widening && quote.is_none() {
                return Err(OrbitError::InvalidInput(
                    "a widening needs your words authorizing it; they are recorded verbatim on the grant request"
                        .to_string(),
                ));
            }
            if !widening {
                return runtime.run_tool_with_session("orbit.resident.grant", input, session);
            }
            // The relaying session marks this caller as other than the
            // native-terminal operator, so core raises the request; the
            // operator then answers it as the human, as the decision view
            // would.
            session.origin_session_id = Some(format!("dashboard:{}", audit_execution_id("resident")));
            if !narrowing {
                let raised = runtime.run_tool_with_session("orbit.resident.grant", input, session)?;
                if raised["outcome"] != "requested" {
                    return Ok(raised);
                }
                let request_id = raised["request"]["id"].as_str().ok_or_else(|| {
                    OrbitError::Execution("the grant request carries no id".to_string())
                })?;
                let answered = runtime.answer_decision_request_as_human(
                    request_id,
                    "grant",
                    Some("Granted from the dashboard Residents view".to_string()),
                    human_comment_author(None),
                    Some("dashboard".to_string()),
                )?;
                let shown = runtime.run_tool_with_session(
                    "orbit.resident.show",
                    json!({"task_id": id}),
                    dashboard_session(true),
                )?;
                return Ok(json!({
                    "outcome": "granted",
                    "request": serde_json::to_value(&answered).unwrap_or(Value::Null),
                    "authority": shown["authority"],
                    "effective_authority": shown["effective_authority"],
                }));
            }
            // Core refuses one call that both widens and narrows, so split
            // a mixed dashboard call into two recorded changes: widen
            // first as an answered request, then narrow.
            let mut widen_input = Map::new();
            widen_input.insert("task_id".into(), json!(id));
            for (key, list) in [("add", &body.add), ("workspaces", &body.workspaces)] {
                if !list.is_empty() {
                    widen_input.insert(key.into(), json!(list));
                }
            }
            if let Some(quote) = &quote {
                widen_input.insert("quote".into(), json!(quote));
            }
            let raised = runtime.run_tool_with_session(
                "orbit.resident.grant",
                Value::Object(widen_input),
                session,
            )?;
            let request = if raised["outcome"] == "requested" {
                let request_id = raised["request"]["id"].as_str().ok_or_else(|| {
                    OrbitError::Execution("the grant request carries no id".to_string())
                })?;
                let answered = runtime.answer_decision_request_as_human(
                    request_id,
                    "grant",
                    Some("Granted from the dashboard Residents view".to_string()),
                    human_comment_author(None),
                    Some("dashboard".to_string()),
                )?;
                serde_json::to_value(&answered).unwrap_or(Value::Null)
            } else {
                raised["request"].clone()
            };
            let mut narrow_input = Map::new();
            narrow_input.insert("task_id".into(), json!(id));
            for (key, list) in [
                ("remove", &body.remove),
                ("drop_workspaces", &body.drop_workspaces),
            ] {
                if !list.is_empty() {
                    narrow_input.insert(key.into(), json!(list));
                }
            }
            if let Some(deadline) = non_blank(body.deadline.as_deref()) {
                narrow_input.insert("deadline".into(), json!(deadline));
            }
            runtime.run_tool_with_session(
                "orbit.resident.grant",
                Value::Object(narrow_input),
                dashboard_session(true),
            )?;
            let shown = runtime.run_tool_with_session(
                "orbit.resident.show",
                json!({"task_id": id}),
                dashboard_session(true),
            )?;
            Ok(json!({
                "outcome": "granted_and_narrowed",
                "request": request,
                "authority": shown["authority"],
                "effective_authority": shown["effective_authority"],
            }))
        },
    )
    .await
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
    )
    .await
}
