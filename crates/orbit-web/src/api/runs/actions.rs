//! Ship and auto-drain workflow actions.

use crate::state::{DashboardState, Ws};
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Json, Response};
use orbit_common::governance::authorization::{
    DASHBOARD_AUTO_DRAIN_APPROVE_PROPOSED, DASHBOARD_AUTO_DRAIN_AUTO_FIX,
    DASHBOARD_AUTO_DRAIN_COMPLETE, DASHBOARD_AUTO_DRAIN_MODE, DASHBOARD_AUTO_DRAIN_STOP,
};
use orbit_common::protocol::tool_input::parse_duration_seconds;
use orbit_core::OrbitRuntime;
use orbit_core::application::job::{DrainAdmissionsStopRequest, SubmissionKind};
use orbit_core::{DrainEntryPoint, WorkspacePullRequest};
use orbit_types::workflow::{JobRunTrigger, ReviewLayer};
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
    /// [ORB-16572] `"before-pr"` or `"before-landing"`: a review layer this
    /// run adds where workspace policy applies none, mirroring `--review`.
    /// Omitted keeps workspace policy.
    #[serde(default)]
    review: Option<ReviewLayer>,
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
        let invoke = runtime.submit_ship_run_with_containment(
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
            // Strict worker containment is a CLI-only flag.
            false,
            body.review,
        )?;
        let mut shipped = json!({
            "workflow": "ship",
            "job_id": invoke.job_name,
            "run_id": invoke.run_id,
            "state": if invoke.queued { "queued" } else { "submitted" },
            "submitted_at": invoke.submitted_at,
        });
        shipped["review"] = runtime.run_review_layer(&invoke.run_id)?;
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
#[serde(deny_unknown_fields)]
pub(in crate::api) struct AutoDrainBody {
    /// Drain window, e.g. "30m", "2h" (CLI `--for`). Required unless
    /// `one_pass` is set, so an empty or misspelled body starts nothing.
    #[serde(default)]
    for_duration: String,
    /// One admission pass: `orbit run auto` with no `--for`. Exclusive with
    /// `for_duration`.
    #[serde(default)]
    one_pass: bool,
    /// Leaf-run concurrency ceiling; omitted keeps the runtime's own default.
    #[serde(default)]
    concurrency: Option<u32>,
    /// Per-drain forecast mode; omitted captures the configured host mode.
    #[serde(default, deserialize_with = "explicit_auto_concurrency")]
    auto_concurrency: Option<orbit_types::workflow::AutoConcurrencyMode>,
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
    /// Explicit repair authority for this owner's PR leaves, mirroring
    /// `--auto-fix`. Off by default; does not authorize completion or merging.
    #[serde(default)]
    auto_fix: bool,
    /// [ORB-16572] Review layer before landing this window adds where
    /// workspace policy applies none, mirroring `--review`; every task it
    /// admits inherits it. It only strengthens review, so it needs no
    /// operator grant. Omitted keeps workspace policy.
    #[serde(default)]
    review: Option<ReviewLayer>,
    /// Crews this window may start (`--allow-crew`). Empty is unrestricted.
    /// The runtime refuses a blank or unconfigured name.
    #[serde(default)]
    allow_crew: Vec<String>,
    /// Per-tier pool overrides (`--<tier>-complexity-crews`): omitted keeps
    /// the workspace pool, `[]` turns the tier's pool off, and a list of
    /// `crew` or `crew:weight` entries replaces it for this window.
    #[serde(default)]
    low_complexity_crews: Option<Vec<String>>,
    #[serde(default)]
    medium_complexity_crews: Option<Vec<String>>,
    #[serde(default)]
    hard_complexity_crews: Option<Vec<String>>,
    #[serde(default)]
    xhard_complexity_crews: Option<Vec<String>>,
    /// `--strict-worker-containment`: require a systemd user scope for the
    /// coordinator and every worker.
    #[serde(default)]
    strict_worker_containment: bool,
    /// `--pull <workspace>`: claim the owner's work from this replica.
    #[serde(default)]
    pull: Option<String>,
    /// `--host <owner>` for `pull`; requires it.
    #[serde(default)]
    host: Option<String>,
    /// [ORB-10709] Token for this workspace's exclusive claim, when another
    /// operator holds one. Never echoed back.
    #[serde(default)]
    claim_token: Option<String>,
}

impl AutoDrainBody {
    fn complexity_crews(&self) -> orbit_core::ComplexityCrewPools {
        orbit_core::ComplexityCrewPools {
            low: self.low_complexity_crews.clone(),
            medium: self.medium_complexity_crews.clone(),
            hard: self.hard_complexity_crews.clone(),
            xhard: self.xhard_complexity_crews.clone(),
        }
    }

    /// The first start option `orbit run auto --pull` refuses, worded as the
    /// CLI's argument parser words it (`AutoCommand`'s `conflicts_with_all`
    /// on `--pull`, then its `--review` refusal), so a pull window refuses
    /// here exactly what the equivalent command refuses.
    fn pull_conflict(&self) -> Option<String> {
        let conflicts = [
            (self.auto_concurrency.is_some(), "--auto-concurrency <MODE>"),
            (self.complete, "--complete"),
            (self.approve_proposed, "--approve-proposed"),
            (self.auto_fix, "--auto-fix"),
            (
                self.strict_worker_containment,
                "--strict-worker-containment",
            ),
            (
                self.low_complexity_crews.is_some(),
                "--low-complexity-crews [<CREW>...]",
            ),
            (
                self.medium_complexity_crews.is_some(),
                "--medium-complexity-crews [<CREW>...]",
            ),
            (
                self.hard_complexity_crews.is_some(),
                "--hard-complexity-crews [<CREW>...]",
            ),
            (
                self.xhard_complexity_crews.is_some(),
                "--xhard-complexity-crews [<CREW>...]",
            ),
            (self.claim_token.is_some(), "--claim-token <CLAIM_TOKEN>"),
        ];
        if let Some((_, flag)) = conflicts.iter().find(|(set, _)| *set) {
            return Some(format!(
                "the argument '--pull <SELECTOR>' cannot be used with '{flag}'"
            ));
        }
        self.review
            .is_some()
            .then(|| orbit_core::application::review::pull_review_request_refusal().to_string())
    }
}

fn explicit_auto_concurrency<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<orbit_types::workflow::AutoConcurrencyMode>, D::Error> {
    <orbit_types::workflow::AutoConcurrencyMode as serde::Deserialize>::deserialize(deserializer)
        .map(Some)
}

/// Minimum window this endpoint accepts for `for_duration`, in seconds. A
/// single pass is asked for explicitly with `one_pass`, so `0s` or a blank
/// duration is rejected rather than silently read as one.
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

/// The window `body` asks for: `None` is one admission pass.
fn drain_window_seconds(body: &AutoDrainBody) -> Result<Option<u64>, String> {
    if body.one_pass {
        if !body.for_duration.trim().is_empty() {
            return Err(
                "one_pass and for_duration are exclusive: one pass is a window with no --for"
                    .to_string(),
            );
        }
        return Ok(None);
    }
    parse_drain_duration_seconds(&body.for_duration).map(Some)
}

/// A refusal the runtime raises before any run exists keeps the words the CLI
/// prints for it. Checkout-role and owner-route refusals are a conflict with
/// this host, not a server fault.
fn map_start_refusal(error: orbit_core::OrbitError) -> Response {
    use orbit_core::OrbitError;
    match error {
        error @ (OrbitError::CapabilityRefused(_)
        | OrbitError::UnknownSelector(_)
        | OrbitError::StaleRoute(_)
        | OrbitError::PolicyDenied(_)) => (
            axum::http::StatusCode::CONFLICT,
            Json(json!({"error": error.to_string(), "code": "start_refused"})),
        )
            .into_response(),
        other => super::super::map_runtime_error(other),
    }
}

/// Submit an `auto` workflow run (`POST /workflows/auto?workspace=<id>`).
///
/// Dashboard counterpart to every start form of `orbit run auto`: a local
/// window (`--for`, `--concurrency`, `--auto-concurrency`, `--allow-crew`,
/// `--<tier>-complexity-crews`, `--complete`, `--approve-proposed`,
/// `--auto-fix`, `--review`, `--strict-worker-containment`, `--claim-token`)
/// or, on a replica, a pull drain (`--pull <workspace> [--host <owner>]` with
/// `--for`, `--concurrency` and `--allow-crew`). It reuses the CLI's own
/// submission paths and owner resolution, so validation and refusals are the
/// runtime's: an unknown crew, a local drain on a replica, a pull on the owner.
/// The flags `--pull` refuses are refused here in the CLI's words before any
/// owner is contacted.
///
/// `?workspace` is required: a missing or blank selector returns 400
/// `workspace_required` and starts nothing, including when the server has a
/// default workspace (`Ws` would otherwise select that default). The window
/// is `for_duration`, or a single pass when `one_pass` is set. Opting into
/// `CompletionPolicy::Done` is explicit and separately governed — it
/// authorizes `review -> done` for every task the window ships, not only the
/// ones visible now, so it is gated the same way `auto_task.mint`'s
/// unconditional mint is. `approve_proposed` (CLI `--approve-proposed`) is
/// likewise explicit and operator-gated: it lets the window approve qualifying
/// proposed tasks, including ones filed after submission. `auto_fix`
/// independently grants operator-only PR repair authority through the
/// existing owner drain, watcher and repair ledger. A non-boolean value fails
/// body parsing like any other bad field. The receipt carries the drain's
/// submission notices (`warning`, `note`, `resource_throttle`,
/// `validation_env_warning`) as `orbit.workflow.auto` does, and never the
/// claim token.
pub(in crate::api) async fn auto_drain_workflow_action(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    Ws(runtime): Ws,
    OptionalJson(body): OptionalJson<AutoDrainBody>,
) -> Response {
    if let Err(rejection) = explicit_workspace(&query) {
        return rejection.into_response();
    }
    let for_seconds = match drain_window_seconds(&body) {
        Ok(seconds) => seconds,
        Err(message) => return bad_request(message),
    };
    if body.pull.is_some() {
        if let Some(conflict) = body.pull_conflict() {
            return bad_request(conflict);
        }
        let global_root = state.global_root().to_path_buf();
        return start_blocking("pull-drain workflow", move || {
            pull_drain_start(&runtime, &global_root, &body, for_seconds)
        })
        .await;
    }
    if body.host.is_some() {
        return bad_request(
            "the following required arguments were not provided: --pull <SELECTOR>".to_string(),
        );
    }
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
    let auto_fix = body.auto_fix;
    if auto_fix
        && let Err(denial) =
            authorized_caller(&DASHBOARD_AUTO_DRAIN_AUTO_FIX, state.operator_session())
    {
        return authorization_denied(denial);
    }
    if body.auto_concurrency.is_some()
        && let Err(denial) = authorized_caller(&DASHBOARD_AUTO_DRAIN_MODE, state.operator_session())
    {
        return authorization_denied(denial);
    }
    start_blocking("auto-drain workflow", move || {
        let invoke = runtime.submit_workspace_auto_run_with_mode(
            for_seconds,
            body.concurrency,
            completion,
            &body.allow_crew,
            &body.complexity_crews(),
            Some("dashboard"),
            body.claim_token.as_deref(),
            JobRunTrigger::dashboard(),
            body.strict_worker_containment,
            approve_proposed,
            auto_fix,
            body.auto_concurrency,
            body.review,
        )?;
        let mut started = json!({
            "workflow": "auto",
            "source": "local",
            "job_id": invoke.job_name,
            "run_id": invoke.run_id,
            "state": if invoke.queued { "queued" } else { "submitted" },
            "submitted_at": invoke.submitted_at,
            "completion": completion.as_input_value(),
            "approve_proposed": approve_proposed,
            "auto_fix": auto_fix,
        });
        started["allowed_crews"] = recorded_allowed_crews(&runtime, &invoke.run_id)?;
        started["auto_concurrency"] = runtime.run_auto_concurrency(&invoke.run_id)?;
        started["review"] = runtime.run_review_layer(&invoke.run_id)?;
        runtime
            .submission_notices(SubmissionKind::Drain)
            .insert_into(&mut started);
        Ok(started)
    })
    .await
}

/// Run a start on the blocking pool, answering with its receipt or with the
/// refusal [`map_start_refusal`] words.
async fn start_blocking<F>(label: &'static str, op: F) -> Response
where
    F: FnOnce() -> Result<Value, orbit_core::OrbitError> + Send + 'static,
{
    match tokio::task::spawn_blocking(op).await {
        Ok(Ok(started)) => Json(started).into_response(),
        Ok(Err(error)) => map_start_refusal(error),
        Err(join_error) => super::super::map_runtime_error(orbit_core::OrbitError::Execution(
            format!("{label} panicked: {join_error}"),
        )),
    }
}

/// `orbit run auto --pull`: resolve the owner selector as the CLI does, then
/// submit through the replica's pull path. The receipt names the selector the
/// window pulls from and, like the CLI, warns when this replica's validation
/// gates drifted from the owner's.
fn pull_drain_start(
    runtime: &OrbitRuntime,
    global_root: &std::path::Path,
    body: &AutoDrainBody,
    for_seconds: Option<u64>,
) -> Result<Value, orbit_core::OrbitError> {
    let pull = body.pull.as_deref().unwrap_or_default();
    let selector = orbit_cmd::hosts::pull_selector(global_root, body.host.as_deref(), pull)?;
    let invoke = runtime.submit_workspace_pull_run(
        WorkspacePullRequest {
            selector: &selector,
            for_seconds,
            max_active_leaf_runs: body.concurrency,
            allowed_crews: &body.allow_crew,
            actor: Some("dashboard"),
        },
        JobRunTrigger::dashboard(),
    )?;
    let mut started = json!({
        "workflow": "auto",
        "source": "pull",
        "selector": selector,
        "job_id": invoke.job_name,
        "run_id": invoke.run_id,
        "state": if invoke.queued { "queued" } else { "submitted" },
        "submitted_at": invoke.submitted_at,
    });
    started["allowed_crews"] = recorded_allowed_crews(runtime, &invoke.run_id)?;
    let notices = runtime.submission_notices(SubmissionKind::PullDrain);
    notices.insert_into(&mut started);
    if let Some(drift) = runtime.pull_gate_drift(&invoke.run_id)? {
        let warning = match started.get("warning").and_then(Value::as_str) {
            Some(existing) => format!("{existing}\n{drift}"),
            None => drift,
        };
        started["warning"] = json!(warning);
    }
    Ok(started)
}

/// The canonical crew names the submitted run recorded, `[]` when it admits
/// every crew.
fn recorded_allowed_crews(
    runtime: &OrbitRuntime,
    run_id: &str,
) -> Result<Value, orbit_core::OrbitError> {
    Ok(runtime
        .show_job_run(run_id)?
        .input
        .as_ref()
        .and_then(|input| input.get("allowed_crews"))
        .cloned()
        .unwrap_or_else(|| json!([])))
}

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
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
    body: Result<OptionalJson<AutoDrainStopBody>, Response>,
) -> Response {
    if let Err(rejection) = explicit_workspace(&query) {
        return rejection.into_response();
    }
    if let Err(denial) = authorized_caller(&DASHBOARD_AUTO_DRAIN_STOP, state.operator_session()) {
        return authorization_denied(denial);
    }
    // Stop is always operator-only, including requests with malformed bodies.
    // Defer the extractor's rejection until after authority is established.
    let body = match body {
        Ok(OptionalJson(body)) => body,
        Err(rejection) => return rejection,
    };
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
    /// Maximum tasks to explain, including an explicit selection (1-500).
    #[serde(default)]
    limit: Option<usize>,
    /// Comma-separated configured crews to evaluate.
    #[serde(default)]
    allow_crew: Option<String>,
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

/// `GET /workflows/auto/readiness?workspace=<id>[&concurrency=N][&task=<id>,…][&limit=N][&allow_crew=<crew>,…]`
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
    let allowed_crews = query
        .allow_crew
        .as_deref()
        .map(orbit_common::protocol::tool_input::split_csv)
        .unwrap_or_default();
    let hosts = state.hosts().snapshot;
    match blocking("auto-drain readiness", move || {
        let mut payload = runtime.workspace_auto_readiness(
            &task_ids,
            query.concurrency,
            query.limit.unwrap_or(AUTO_DRAIN_READINESS_LIMIT),
            &allowed_crews,
        )?;
        payload["start_options"] = drain_start_options(
            &runtime,
            hosts.as_deref().map(|snapshot| &snapshot.registry),
        )?;
        Ok(payload)
    })
    .await
    {
        Ok(mut payload) => {
            // Findings are durable task data. The card needs only counts and
            // reasons; CLI readiness keeps the full per-task readout.
            if let Some(held) = payload
                .get_mut("approvals")
                .and_then(|approvals| approvals.get_mut("held"))
                .and_then(Value::as_array_mut)
            {
                for task in held {
                    if let Some(task) = task.as_object_mut() {
                        task.remove("findings");
                    }
                }
            }
            // So the form can hide/disable the `complete` and
            // `approve_proposed`, `auto_fix` and forecast-mode opt-ins and the stop control before the
            // operator ever hits the separately-governed 403 at submission.
            // All five operations admit the same operator capability, so one
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
                            && authorized_caller(&DASHBOARD_AUTO_DRAIN_AUTO_FIX, operator_session)
                                .is_ok()
                            && authorized_caller(&DASHBOARD_AUTO_DRAIN_STOP, operator_session)
                                .is_ok()
                            && authorized_caller(&DASHBOARD_AUTO_DRAIN_MODE, operator_session)
                                .is_ok(),
                    ),
                );
            }
            Json(payload).into_response()
        }
        Err(response) => *response,
    }
}

/// What the Start a window composer offers on this checkout: whether it is
/// the workspace's owner (a local drain) or a replica (a pull drain from its
/// owner, with the owner's host name and the selector `--pull` resolves to),
/// the runtime's own refusal for the source this checkout cannot start, the
/// configured crews for `--allow-crew`, and the workspace pools each
/// `--<tier>-complexity-crews` override replaces. A pool configuration the
/// runtime cannot read leaves `complexity_pools` null; submission reports why.
fn drain_start_options(
    runtime: &OrbitRuntime,
    hosts: Option<&orbit_registry::hosts::HostRegistry>,
) -> Result<Value, orbit_core::OrbitError> {
    let workspace_id = runtime
        .workspace_runtime_binding()
        .map(|binding| binding.logical_workspace_id.clone());
    let owner = runtime.replica_owner_machine().map(str::to_string);
    let owner_host = owner.as_deref().and_then(|machine_id| {
        hosts?
            .hosts()
            .entries()
            .iter()
            .find(|entry| entry.machine_id == machine_id)
            .map(|entry| entry.name.clone())
    });
    let local_refusal = match owner {
        Some(_) => runtime
            .drain_entry_admission(DrainEntryPoint::OwnerDrain, &[], false)?
            .into_result()
            .err()
            .map(|error| error.to_string()),
        None => None,
    };
    let crews = runtime
        .configured_crew_registry_projection()
        .crews
        .into_iter()
        .map(|crew| json!({"name": crew.name, "enabled": crew.enabled}))
        .collect::<Vec<_>>();
    let complexity_pools = runtime
        .workspace_complexity_crew_pools()
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "workspace complexity pools unreadable; the composer shows none");
            Value::Null
        });
    Ok(json!({
        "checkout": if owner.is_some() { "replica" } else { "owner" },
        "workspace_id": workspace_id,
        "this_host": hosts.and_then(|hosts| hosts.local()).map(|local| local.name.clone()),
        "owner_machine_id": owner,
        "owner_host": owner_host,
        "pull_selector": owner
            .as_deref()
            .zip(workspace_id.as_deref())
            .map(|(machine, workspace)| format!("{machine}/{workspace}")),
        "local_refusal": local_refusal,
        "pull_refusal": runtime
            .pull_drain_checkout_refusal()
            .map(|error| error.to_string()),
        "crews": crews,
        "complexity_pools": complexity_pools,
    }))
}
