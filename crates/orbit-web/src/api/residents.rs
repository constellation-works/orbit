//! The Automation › Residents view: set up and manage resident mandates.
//!
//! Reads report exactly what `orbit resident list|show` do, and writes go
//! through the `orbit.resident.*` tools, so the view changes exactly what
//! `orbit resident list|show|relay|control` do. The list read uses the
//! batched core card read instead of per-row tool calls. Two writes go to
//! core use cases instead. Creation needs a file for
//! `orbit.task.artifact` action `put`; its use case judges the draft with the same
//! rule every mandate read applies before it writes the tagged assignment
//! and its `resident/mandate.json`. The grant form goes to the dashboard
//! grant operation beside `orbit.resident.grant`'s, which applies the same
//! grant rules.
//!
//! * `GET /api/residents` — every mandate as `orbit.resident.show` reports
//!   it, the configured crews for the form, the workspace's resident cycle
//!   routine and what this session may do. Each mandate also carries
//!   `cycle_authority` and `open_decisions` for the card.
//! * `GET /api/residents/<id>` — one mandate, with the same two card reads.
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

use std::collections::{BTreeMap, BTreeSet};
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
use orbit_types::workflow::{RESIDENT_CYCLE_GRANTED_EVENT, ResidentCycleGrant, ResidentRight};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::routines::{
    OperationAudit, action_capability, authorization_denied, authorized_caller, status_json,
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

/// Card reads the show payload does not carry.
///
/// `cycle_authority` is the grant history recorded for the live cycle, or
/// for the latest checkpoint's cycle when nothing is live. A live cycle
/// with no grant stays null: an older checkpoint must not be labeled as
/// that cycle. `open_decisions` is every open decision on the mandate,
/// plus a checkpoint request that is still open on another task. A failed
/// read is null so the card can fall back to the grant requests show
/// already returned.
fn attach_resident_card(runtime: &OrbitRuntime, shown: &mut Value) {
    if !shown.is_object() {
        return;
    }
    let Some(task_id) = shown
        .get("task_id")
        .and_then(Value::as_str)
        .filter(|task_id| !task_id.is_empty())
        .map(str::to_owned)
    else {
        return;
    };
    shown["open_decisions"] = open_decisions_for_card(runtime, &task_id, shown);
    shown["cycle_authority"] = cycle_authority_for_card(runtime, &task_id, shown);
}

fn open_decisions_for_card(runtime: &OrbitRuntime, task_id: &str, shown: &Value) -> Value {
    let requests = match runtime.list_decision_requests(None, Some(task_id), false) {
        Ok(requests) => requests,
        Err(error) => {
            tracing::warn!(%error, %task_id, "resident card could not list open decisions");
            return Value::Null;
        }
    };
    let mut seen = BTreeSet::new();
    let mut values = Vec::new();
    for request in requests {
        if seen.insert(request.id.clone()) {
            push_decision_json(&mut values, &request);
        }
    }
    for (other_task, decision_ids) in checkpoint_decisions_on_other_tasks(shown, task_id, &seen) {
        let found = match runtime.list_decision_requests(None, Some(&other_task), false) {
            Ok(found) => found,
            Err(error) => {
                tracing::warn!(
                    %error,
                    task = %other_task,
                    "resident card could not read a checkpoint decision"
                );
                continue;
            }
        };
        for request in found {
            if decision_ids.iter().any(|id| id == &request.id) && seen.insert(request.id.clone()) {
                push_decision_json(&mut values, &request);
            }
        }
    }
    Value::Array(values)
}

fn push_decision_json(values: &mut Vec<Value>, request: &impl serde::Serialize) {
    match serde_json::to_value(request) {
        Ok(value) => values.push(value),
        Err(error) => {
            tracing::warn!(%error, "resident card skipped a decision it could not render");
        }
    }
}

/// Checkpoint requests whose decision lives on another task, grouped by that task.
fn checkpoint_decisions_on_other_tasks(
    shown: &Value,
    task_id: &str,
    seen: &BTreeSet<String>,
) -> BTreeMap<String, Vec<String>> {
    let mut by_task = BTreeMap::new();
    let Some(rows) = shown
        .get("latest_checkpoint")
        .and_then(|checkpoint| checkpoint.get("requests"))
        .and_then(Value::as_array)
    else {
        return by_task;
    };
    for row in rows {
        let Some(decision_id) = row
            .get("decision_request")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        let Some(other_task) = row
            .get("task")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && *id != task_id)
        else {
            continue;
        };
        if seen.contains(decision_id) {
            continue;
        }
        let ids = by_task.entry(other_task.to_string()).or_default();
        if !ids.iter().any(|id| id == decision_id) {
            ids.push(decision_id.to_string());
        }
    }
    by_task
}

fn cycle_authority_for_card(runtime: &OrbitRuntime, task_id: &str, shown: &Value) -> Value {
    let Some((target, live)) = cycle_authority_target(shown) else {
        return Value::Null;
    };
    let history = match runtime.get_task_history(task_id) {
        Ok(history) => history,
        Err(error) => {
            tracing::warn!(%error, %task_id, "resident card could not read cycle authority");
            return Value::Null;
        }
    };
    if let Some(grant) = history.iter().rev().find_map(|entry| {
        if entry.event != RESIDENT_CYCLE_GRANTED_EVENT {
            return None;
        }
        let grant = serde_json::from_str::<ResidentCycleGrant>(entry.note.as_deref()?).ok()?;
        (grant.cycle == target).then_some(grant)
    }) {
        return grant_authority_json(&grant, live);
    }
    if live {
        return Value::Null;
    }
    reported_cycle_authority(shown, &target)
}

/// The live cycle when one is running, otherwise the latest checkpoint's cycle.
fn cycle_authority_target(shown: &Value) -> Option<(String, bool)> {
    if let Some(cycle) = shown
        .get("live_cycle")
        .and_then(|live| live.get("cycle"))
        .and_then(Value::as_str)
        .filter(|cycle| !cycle.is_empty())
    {
        return Some((cycle.to_string(), true));
    }
    let cycle = shown
        .get("latest_checkpoint")
        .and_then(|checkpoint| checkpoint.get("cycle"))
        .and_then(Value::as_str)
        .filter(|cycle| !cycle.is_empty())?;
    Some((cycle.to_string(), false))
}

fn grant_authority_json(grant: &ResidentCycleGrant, live: bool) -> Value {
    let rights: Vec<&str> = grant
        .rights
        .iter()
        .copied()
        .map(ResidentRight::as_str)
        .collect();
    let mut body = Map::new();
    body.insert("cycle".into(), json!(grant.cycle));
    body.insert("live".into(), json!(live));
    body.insert("rights".into(), json!(rights));
    body.insert("workspaces".into(), json!(grant.workspaces));
    body.insert("observe_only".into(), json!(grant.observe_only));
    if let Some(narrowed_by) = &grant.narrowed_by {
        body.insert("narrowed_by".into(), json!(narrowed_by));
    }
    Value::Object(body)
}

/// The checkpoint's own authority report, only for the cycle it names.
/// Workspaces are omitted: the report does not record them.
fn reported_cycle_authority(shown: &Value, target: &str) -> Value {
    let Some(checkpoint) = shown.get("latest_checkpoint") else {
        return Value::Null;
    };
    if checkpoint.get("cycle").and_then(Value::as_str) != Some(target) {
        return Value::Null;
    }
    let Some(authority) = checkpoint
        .get("authority")
        .filter(|value| value.is_object())
    else {
        return Value::Null;
    };
    let rights = authority
        .get("rights")
        .filter(|value| value.is_array())
        .cloned()
        .unwrap_or_else(|| json!([]));
    let mut body = Map::new();
    body.insert("cycle".into(), json!(target));
    body.insert("live".into(), json!(false));
    body.insert("rights".into(), rights);
    body.insert(
        "observe_only".into(),
        json!(
            authority
                .get("observe_only")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        ),
    );
    if let Some(narrowed_by) = authority.get("narrowed_by").and_then(Value::as_str) {
        body.insert("narrowed_by".into(), json!(narrowed_by));
    }
    body.insert("reported".into(), json!(true));
    Value::Object(body)
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
        let mandates = runtime.list_resident_mandate_cards(include_ended)?;
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
        let mut shown = runtime.run_tool_with_session(
            "orbit.resident.show",
            json!({"task_id": id}),
            dashboard_session(operator),
        )?;
        attach_resident_card(&runtime, &mut shown);
        Ok(shown)
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
    let audit = OperationAudit::new(&runtime, &workspace, operation, target, arguments);
    let caller = match authorized_caller(&DASHBOARD_RESIDENT_WRITE, state.operator_session()) {
        Ok(caller) => caller,
        Err(denial) => {
            audit.denied(&denial, started).await;
            return authorization_denied(denial);
        }
    };
    let writer = Arc::clone(&runtime);
    let result = blocking(operation, move || write(writer)).await;
    let failure: Option<String> = match &result {
        Err(_) => Some("refused".to_string()),
        Ok(value) => partial_failure(value),
    };
    audit.settled(&caller, failure.as_deref(), started).await;
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
