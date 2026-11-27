//! The Automation › Residents view: set up and manage resident mandates.
//!
//! The show read reports what `orbit resident show` does, except where the
//! checkpoint comment time sits (see the paragraph after the routes below).
//! Each list card carries that same show payload; `orbit resident list` rows
//! do not.
//! Every read and write is one command of the resident app
//! (`orbit_core::app::resident`). Its channel writes go through the
//! `orbit.resident.*` tools, so the view changes exactly what `orbit resident
//! list|show|relay|control` do. The list read uses the batched core card read
//! instead of per-row tool calls. Two writes go to core use cases instead.
//! Creation needs a file for `orbit.task.artifact` action `put`; its use case
//! judges the draft with the same rule every mandate read applies before it
//! writes the tagged assignment and its `resident/mandate.json`. The grant form
//! goes to the dashboard grant operation beside `orbit.resident.grant`'s, which
//! applies the same grant rules.
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
//! The checkpoint comment time has a different path on each surface. The
//! core read and `orbit.resident.show` (and so `orbit resident show --json`)
//! report it top-level as `latest_checkpoint_at`, null when the mandate has
//! no checkpoint, and leave `latest_checkpoint` as the parsed record. Both
//! GET reads move it into `latest_checkpoint.at`, the persisted time of the
//! latest checkpoint comment (not the cycle id's minute), and drop the
//! top-level field. With no checkpoint, `latest_checkpoint` is null and has
//! no `at`.
//!
//! Every write needs the operator session ([`DASHBOARD_RESIDENT_WRITE`]),
//! which the resident app decides, and leaves a `dashboard.operations` audit
//! row beside the history entry core writes. Turning the cycle routine on or off is the audited
//! `POST /api/routines/toggle`.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Json, Response};
use chrono::Utc;
use orbit_cmd::registry_routines::routine_statuses_for_workspace;
use orbit_common::governance::authorization::{DASHBOARD_RESIDENT_WRITE, DASHBOARD_ROUTINE_TOGGLE};
use orbit_core::app::resident::{
    Command, CycleRoutineScope, DashboardWrite, DashboardWritten, Outcome,
    ResidentDashboardGrantParams, exec,
};
use orbit_core::{Core, OrbitError};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::blocking;
use super::routines::{action_capability, authorization_denied, dashboard_caller, status_json};
use super::tasks::human_comment_author;
use crate::state::{DashboardState, WsCore};

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

fn non_blank(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// The workspace's resident cycle routine row, as Routines lists it.
///
/// Reads routine status for this workspace (checking only peer workspaces
/// whose definitions declare a colliding routine name): a full host-wide read
/// loads every registered workspace's definitions, configuration and store
/// rows, which this one row never needs.
fn resident_cycle_routine(state: &DashboardState, scope: Option<&CycleRoutineScope>) -> Value {
    let Some(scope) = scope else {
        return Value::Null;
    };
    let Ok(report) = routine_statuses_for_workspace(state.global_root(), &scope.workspace_id)
    else {
        return Value::Null;
    };
    // The routine's source is the registered workspace whose `.orbit`
    // directory it was loaded from: this workspace's.
    let canonical = |path: &std::path::Path| std::fs::canonicalize(path).ok();
    let orbit_dir = canonical(&scope.orbit_dir);
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

/// Put the checkpoint comment time on the HTTP card without changing the
/// parsed checkpoint record returned by the resident read model.
fn expose_latest_checkpoint_at(shown: &mut Value) {
    let latest_checkpoint_at = shown
        .as_object_mut()
        .and_then(|object| object.remove("latest_checkpoint_at"));
    if let Some(at) = latest_checkpoint_at.filter(|at| !at.is_null())
        && let Some(checkpoint) = shown
            .get_mut("latest_checkpoint")
            .filter(|checkpoint| checkpoint.is_object())
    {
        checkpoint["at"] = at;
    }
}

/// `GET /api/residents`.
pub(super) async fn list_residents(
    State(state): State<DashboardState>,
    WsCore(core): WsCore,
    Query(query): Query<ResidentListQuery>,
) -> Response {
    let operator = state.operator_session();
    let include_ended = query.all;
    match blocking("resident list", move || {
        let cards = match exec(&core, Command::Cards { include_ended })? {
            Outcome::Cards(cards) => *cards,
            Outcome::Value(_) | Outcome::Dashboard(_) => {
                return Err(unexpected_outcome("resident list"));
            }
        };
        let mut mandates = cards.mandates;
        for mandate in &mut mandates {
            expose_latest_checkpoint_at(mandate);
        }
        let crews = cards.crews;
        let crew_names: Vec<Value> = crews["crews"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|crew| crew.get("name").cloned())
            .collect();
        Ok(json!({
            "workspace": cards.workspace,
            "count": mandates.len(),
            "mandates": mandates,
            "crews": crew_names,
            "default_crew": crews["default_crew"],
            "routine": resident_cycle_routine(&state, cards.cycle_routine.as_ref()),
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
    WsCore(core): WsCore,
    Path(id): Path<String>,
) -> Response {
    let operator = state.operator_session();
    match blocking("resident show", move || {
        let mut shown = exec(
            &core,
            Command::Card {
                task_id: id,
                operator,
            },
        )?
        .into_value();
        expose_latest_checkpoint_at(&mut shown);
        Ok(shown)
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

/// Send one resident write to the resident app, which authorizes it, runs
/// it and audits the outcome, and render what it answered.
async fn resident_write(
    state: &DashboardState,
    core: Arc<Core>,
    label: &'static str,
    write: DashboardWrite,
) -> Response {
    let caller = dashboard_caller(state, None, "");
    match blocking(label, move || {
        exec(&core, Command::Dashboard(caller, write))
    })
    .await
    {
        Ok(Outcome::Dashboard(Ok(DashboardWritten::Value(value)))) => Json(value).into_response(),
        Ok(Outcome::Dashboard(Ok(DashboardWritten::Granted(grant)))) => {
            Json(*grant).into_response()
        }
        Ok(Outcome::Dashboard(Err(denial))) => authorization_denied(denial),
        Ok(Outcome::Value(_) | Outcome::Cards(_)) => super::server_error(unexpected_outcome(label)),
        Err(response) => *response,
    }
}

/// An app answered one command with another command's outcome.
fn unexpected_outcome(label: &str) -> OrbitError {
    OrbitError::Execution(format!("{label} answered with another command's outcome"))
}

/// The fields of a resident tool call.
fn tool_fields(input: Value) -> Map<String, Value> {
    match input {
        Value::Object(fields) => fields,
        _ => Map::new(),
    }
}

/// `POST /api/residents` — create a mandate's assignment and artifact.
pub(super) async fn create_resident(
    State(state): State<DashboardState>,
    WsCore(core): WsCore,
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
    resident_write(
        &state,
        core,
        "resident.create",
        DashboardWrite::Create {
            title,
            description: body.description.unwrap_or_default(),
            complexity: body.complexity,
            mandate,
        },
    )
    .await
}

/// `POST /api/residents/<id>/grant` — narrow, widen as an answered
/// request, or both, through the resident app's dashboard grant. The
/// handler parses the form and renders the core result; the app authorizes
/// and audits it, and a partial outcome audits as a failure naming which
/// step applied.
pub(super) async fn grant_resident(
    State(state): State<DashboardState>,
    WsCore(core): WsCore,
    Path(id): Path<String>,
    Json(body): Json<ResidentGrantBody>,
) -> Response {
    let params = ResidentDashboardGrantParams {
        task_id: id,
        add: body.add,
        workspaces: body.workspaces,
        remove: body.remove,
        drop_workspaces: body.drop_workspaces,
        deadline: body.deadline,
        quote: body.quote,
        answered_by: human_comment_author(None),
    };
    let label = if params.widens() {
        "resident.grant"
    } else {
        "resident.narrow"
    };
    resident_write(&state, core, label, DashboardWrite::Grant(params)).await
}

/// `POST /api/residents/<id>/control`.
pub(super) async fn control_resident(
    State(state): State<DashboardState>,
    WsCore(core): WsCore,
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
    resident_write(
        &state,
        core,
        "resident.control",
        DashboardWrite::Control(tool_fields(input)),
    )
    .await
}

/// `POST /api/residents/<id>/relay` — the operator's own words, said here.
pub(super) async fn relay_resident(
    State(state): State<DashboardState>,
    WsCore(core): WsCore,
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
    resident_write(
        &state,
        core,
        "resident.relay",
        DashboardWrite::Relay(tool_fields(input)),
    )
    .await
}
