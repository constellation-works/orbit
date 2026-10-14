//! Ship and auto-drain workflow actions.

use crate::state::{DashboardState, Ws};
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Json, Response};
use orbit_common::governance::authorization::{
    DASHBOARD_AUTO_DRAIN_APPROVE_PROPOSED, DASHBOARD_AUTO_DRAIN_COMPLETE, DASHBOARD_AUTO_DRAIN_STOP,
};
use orbit_common::protocol::tool_input::parse_duration_seconds;
use orbit_core::OrbitRuntime;
use orbit_core::application::job::{DrainAdmissionsStopRequest, SubmissionKind};
use orbit_types::workflow::JobRunTrigger;
use serde_json::{Value, json};

use super::super::routines::{
    OperationsQuery, authorization_denied, authorized_caller, explicit_workspace,
};
use super::super::{OptionalJson, bad_request, blocking, validate_id};

/// Request body for `POST /workflows/ship`.
///
/// Unknown keys are rejected. A misspelled selector such as `task_id` must
/// not deserialize as an empty `task_ids`: an empty selection is
/// backlog-discovery mode and would ship every eligible task.
#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(in crate::api) struct ShipBody {
    /// Explicit task selection; empty selects auto (backlog-discovery) mode.
    #[serde(default)]
    task_ids: Vec<String>,
    /// `"pr"` or `"local"`. Omitted means the workspace's own configured ship
    /// mode (ORB-10444) — what the dashboard's one-click Ship sends.
    #[serde(default)]
    mode: Option<String>,
    /// Base branch override; defaults to the registered workspace base
    /// branch, else `[workflow] base_branch`.
    #[serde(default)]
    base: Option<String>,
    /// [ORB-10709] Token for this workspace's exclusive claim, when another
    /// operator holds one.
    #[serde(default)]
    claim_token: Option<String>,
}

/// Submit a `ship` workflow run (`POST /workflows/ship?workspace=<id>`).
///
/// One-shot: responds as soon as the run is persisted, with the same
/// `queued`/`submitted` states the CLI reports, and the same submission
/// notices (`warning`, `validation_env_warning`) `orbit run ship` and
/// `orbit.workflow.ship` report. Callers poll `GET /runs/:id` for progress.
///
/// An omitted `mode` resolves to the selected workspace's own ship mode
/// (ORB-10444), so the dashboard's one-click Ship needs no PR/local toggle;
/// a workspace with no registry binding (single/embedded mode) keeps the
/// historical `pr` default.
///
/// [ORB-10544] Duplicate dispatch of an explicitly-selected task is refused by
/// the shared submission path, not here: `submit_ship_run` returns
/// `OrbitError::ShipRunInFlight` when one of the named tasks is already carried
/// by a non-terminal delivery run, which `map_runtime_error` projects to this
/// endpoint's stable 409. Auto (backlog-discovery) mode has no task ids to guard
/// and is unaffected.
pub(in crate::api) async fn ship_workflow_action(
    Ws(runtime): Ws,
    OptionalJson(body): OptionalJson<ShipBody>,
) -> Response {
    let mode = match body.mode.as_deref() {
        Some(raw) => match orbit_core::ShipMode::parse(raw) {
            Ok(mode) => mode,
            Err(e) => return bad_request(e.to_string()),
        },
        None => workspace_default_ship_mode(&runtime),
    };
    match blocking("ship workflow", move || {
        let invoke = runtime.submit_ship_run(
            mode,
            body.base.as_deref(),
            &body.task_ids,
            // [ORB-11187] Completion authority is granted per invocation at the CLI
            // (`orbit run ship --complete`); the dashboard does not offer it, so
            // this endpoint always ends successful work at `review`.
            orbit_core::CompletionPolicy::Review,
            &[],
            Some("dashboard"),
            body.claim_token.as_deref(),
            JobRunTrigger::dashboard(),
        )?;
        let mut shipped = json!({
            "workflow": "ship",
            "job_id": invoke.job_name,
            "run_id": invoke.run_id,
            "state": if invoke.queued { "queued" } else { "submitted" },
            "submitted_at": invoke.submitted_at,
        });
        runtime
            .submission_notices(SubmissionKind::Ship {
                explicit: !body.task_ids.is_empty(),
            })
            .insert_into(&mut shipped);
        Ok(shipped)
    })
    .await
    {
        Ok(shipped) => Json(shipped).into_response(),
        Err(response) => *response,
    }
}

/// The selected workspace's configured ship mode, or `pr` when this runtime was
/// built without a registry binding (single/embedded serving mode), which is the
/// default the endpoint has always applied to a body with no `mode`.
fn workspace_default_ship_mode(runtime: &OrbitRuntime) -> orbit_core::ShipMode {
    runtime
        .workspace_runtime_binding()
        .map_or(orbit_core::ShipMode::Pr, |binding| binding.ship_mode)
}

#[derive(serde::Deserialize, Default)]
pub(in crate::api) struct AutoDrainBody {
    /// Bounded drain window, e.g. "30m", "2h". Required: unlike the CLI's
    /// `--for`, this endpoint always starts a bounded window, never an
    /// open-ended one left running until something else stops it.
    #[serde(default)]
    for_duration: String,
    /// Leaf-run concurrency ceiling; omitted keeps the runtime's own default.
    #[serde(default)]
    concurrency: Option<u32>,
    /// Opt-in to `CompletionPolicy::Done` for this run's whole window,
    /// mirroring CLI `--complete`. Default keeps every shipped task at
    /// `review`, same as an omitted `--complete`.
    #[serde(default)]
    complete: bool,
    /// Opt-in to approving qualifying `proposed` tasks on every pass of this
    /// window, mirroring CLI `--approve-proposed`. Default leaves proposed
    /// tasks for the operator, same as an omitted flag.
    #[serde(default)]
    approve_proposed: bool,
    /// [ORB-10709] Token for this workspace's exclusive claim, when another
    /// operator holds one.
    #[serde(default)]
    claim_token: Option<String>,
}

/// Minimum bounded window this endpoint accepts, in seconds. `for_seconds:
/// 0`/omitted is a CLI-only "one tick" shorthand; the dashboard's whole point
/// is a *bounded* window, so an empty one is rejected rather than silently
/// accepted.
const MIN_AUTO_DRAIN_SECONDS: u64 = 1;

/// Read-only eligible/waiting projection for backlog-discovery auto-drain,
/// same limit the CLI's `orbit run readiness` defaults to.
const AUTO_DRAIN_READINESS_LIMIT: usize = 50;

/// Parses a "30m"/"2h"/"1d"-shaped duration the same way the CLI's `--for`
/// does, then requires a non-empty window.
fn parse_drain_duration_seconds(raw: &str) -> Result<u64, String> {
    if raw.trim().is_empty() {
        return Err("for_duration must not be empty".to_string());
    }
    let seconds = parse_duration_seconds(raw).map_err(|error| match error {
        orbit_core::OrbitError::InvalidInput(message) => message,
        other => other.to_string(),
    })?;
    if seconds < MIN_AUTO_DRAIN_SECONDS {
        return Err("for_duration must describe a bounded window greater than zero".to_string());
    }
    Ok(seconds)
}

/// Submit a bounded `auto` workflow run
/// (`POST /workflows/auto?workspace=<id>`).
///
/// Dashboard counterpart to `orbit run auto --for <duration> [--concurrency
/// N] [--complete] [--approve-proposed]`, reusing the same drain-submission
/// runtime path the CLI does. `?workspace` is required: a missing or blank selector returns 400
/// `workspace_required` and starts nothing, including when the server has a
/// default workspace (`Ws` would otherwise select that default).
/// `submit_workspace_auto_run` still enforces the workspace claim. The
/// duration must be a bounded window, and opting into `CompletionPolicy::Done`
/// is explicit and separately governed — it authorizes `review -> done` for
/// every task the window ships, not only the ones visible now, so it is gated
/// the same way `auto_task.mint`'s unconditional mint is. `approve_proposed`
/// (CLI `--approve-proposed`) is likewise explicit and operator-gated: it lets
/// the window approve qualifying proposed tasks, including ones filed after
/// submission. A non-boolean value fails body parsing like any other bad field.
/// The receipt carries the drain's submission notices (`warning`, `note`,
/// `resource_throttle`, `validation_env_warning`) as `orbit.workflow.auto`
/// does.
pub(in crate::api) async fn auto_drain_workflow_action(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    Ws(runtime): Ws,
    OptionalJson(body): OptionalJson<AutoDrainBody>,
) -> Response {
    if let Err(rejection) = explicit_workspace(&query) {
        return rejection.into_response();
    }
    let for_seconds = match parse_drain_duration_seconds(&body.for_duration) {
        Ok(seconds) => seconds,
        Err(message) => return bad_request(message),
    };
    let completion = if body.complete {
        match authorized_caller(&DASHBOARD_AUTO_DRAIN_COMPLETE, state.operator_session()) {
            Ok(_) => orbit_core::CompletionPolicy::Done,
            Err(denial) => return authorization_denied(denial),
        }
    } else {
        orbit_core::CompletionPolicy::Review
    };
    let approve_proposed = body.approve_proposed;
    if approve_proposed
        && let Err(denial) = authorized_caller(
            &DASHBOARD_AUTO_DRAIN_APPROVE_PROPOSED,
            state.operator_session(),
        )
    {
        return authorization_denied(denial);
    }
    match blocking("auto-drain workflow", move || {
        let invoke = runtime.submit_workspace_auto_run_with_containment(
            Some(for_seconds),
            body.concurrency,
            completion,
            // [ORB-11242] The dashboard launch form does not offer a crew
            // restriction, so it submits the unrestricted window it always has.
            &[],
            &Default::default(),
            Some("dashboard"),
            body.claim_token.as_deref(),
            JobRunTrigger::dashboard(),
            // Strict worker containment is a CLI-only flag.
            false,
            approve_proposed,
            // `--auto-fix` is a CLI-only flag.
            false,
        )?;
        let mut started = json!({
            "workflow": "auto",
            "job_id": invoke.job_name,
            "run_id": invoke.run_id,
            "state": if invoke.queued { "queued" } else { "submitted" },
            "submitted_at": invoke.submitted_at,
            "completion": completion.as_input_value(),
            "approve_proposed": approve_proposed,
        });
        runtime
            .submission_notices(SubmissionKind::Drain)
            .insert_into(&mut started);
        Ok(started)
    })
    .await
    {
        Ok(started) => Json(started).into_response(),
        Err(response) => *response,
    }
}

#[derive(serde::Deserialize, Default)]
pub(in crate::api) struct AutoDrainStopBody {
    /// Free-text reason recorded on the coordinator's stop marker and audit.
    #[serde(default)]
    reason: Option<String>,
    /// [ORB-10709] Token for this workspace's exclusive claim, when another
    /// operator holds one.
    #[serde(default)]
    claim_token: Option<String>,
}

/// Stop new admissions for one workspace's live `auto` window
/// (`POST /workflows/auto/stop?workspace=<id>`) [ORB-12728].
///
/// `?workspace` is required. A missing or blank selector returns 400
/// `workspace_required` and stops nothing, including when the server has a
/// default workspace. `Ws` would otherwise fall back to that default; this
/// handler does not.
///
/// Dashboard counterpart to `orbit run auto --stop`, reusing
/// `stop_workspace_auto_admissions`: every live coordinator in this concrete
/// workspace stops admitting, already admitted workers keep running under
/// their captured completion authority, and a workspace with no coordinator
/// reports `idle`. On a replica it also delivers every recorded pull
/// settlement [ORB-13663]. This is not cancellation. Ending an unattended delivery
/// window early is an operator decision, so it is gated like the other
/// governed dashboard controls (`--operator` session) and refused before any
/// runtime call.
pub(in crate::api) async fn auto_drain_stop_action(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    Ws(runtime): Ws,
    OptionalJson(body): OptionalJson<AutoDrainStopBody>,
) -> Response {
    if let Err(rejection) = explicit_workspace(&query) {
        return rejection.into_response();
    }
    if let Err(denial) = authorized_caller(&DASHBOARD_AUTO_DRAIN_STOP, state.operator_session()) {
        return authorization_denied(denial);
    }
    match blocking("auto-drain stop", move || {
        runtime.stop_workspace_auto_admissions(DrainAdmissionsStopRequest {
            actor: "dashboard",
            source: "dashboard",
            reason: body.reason.as_deref(),
            claim_token: body.claim_token.as_deref(),
            force: false,
        })
    })
    .await
    {
        Ok(result) => {
            let mut stopped = result.to_json();
            stopped["workflow"] = json!("auto");
            Json(stopped).into_response()
        }
        Err(response) => *response,
    }
}

#[derive(serde::Deserialize, Default)]
pub(in crate::api) struct AutoDrainReadinessQuery {
    #[serde(default)]
    concurrency: Option<u32>,
    /// Comma-separated task ids to explain instead of the bounded backlog
    /// listing, so a task detail gets its own answer wherever it ranks.
    #[serde(default)]
    task: Option<String>,
}

/// The explicit selection a `?task=` value names; empty when it is absent.
fn readiness_task_ids(raw: Option<&str>) -> Result<Vec<String>, String> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    raw.split(',')
        .map(|id| validate_id(id.trim()).map(str::to_string))
        .collect()
}

/// `GET /workflows/auto/readiness?workspace=<id>[&concurrency=N][&task=<id>,…]`
/// — the read-only eligible/waiting snapshot for backlog-discovery
/// auto-drain, projected straight from `OrbitRuntime::workspace_auto_readiness`
/// (the same snapshot `orbit run readiness` prints) rather than a
/// dashboard-local recomputation of eligibility. `task` explains exactly
/// those tasks, as `orbit run readiness <id>…` does; an unknown id is a 400.
pub(in crate::api) async fn auto_drain_readiness(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Query(query): Query<AutoDrainReadinessQuery>,
) -> Response {
    let task_ids = match readiness_task_ids(query.task.as_deref()) {
        Ok(ids) => ids,
        Err(message) => return bad_request(message),
    };
    match blocking("auto-drain readiness", move || {
        let mut payload = runtime.workspace_auto_readiness(
            &task_ids,
            query.concurrency,
            AUTO_DRAIN_READINESS_LIMIT,
            &[],
        )?;
        // A replica executes through a pull drain, which cannot approve
        // proposed tasks (`--pull` conflicts with `--approve-proposed`), so the
        // form withholds that opt-in there.
        if let Some(object) = payload.as_object_mut() {
            object.insert(
                "replica".to_string(),
                Value::Bool(runtime.replica_owner_machine().is_some()),
            );
        }
        Ok(payload)
    })
    .await
    {
        Ok(mut payload) => {
            // So the form can hide/disable the `complete` and
            // `approve_proposed` opt-ins and the stop control before the
            // operator ever hits the separately-governed 403 at submission.
            // All three operations admit the same operator capability, so one
            // flag describes them.
            if let Some(object) = payload.as_object_mut() {
                let operator_session = state.operator_session();
                object.insert(
                    "controls_authorized".to_string(),
                    Value::Bool(
                        authorized_caller(&DASHBOARD_AUTO_DRAIN_COMPLETE, operator_session).is_ok()
                            && authorized_caller(
                                &DASHBOARD_AUTO_DRAIN_APPROVE_PROPOSED,
                                operator_session,
                            )
                            .is_ok()
                            && authorized_caller(&DASHBOARD_AUTO_DRAIN_STOP, operator_session)
                                .is_ok(),
                    ),
                );
            }
            Json(payload).into_response()
        }
        Err(response) => *response,
    }
}
