//! Ship and auto-drain workflow actions.

use crate::api::helpers;
use crate::state::{DashboardState, WsCore};
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Json, Response};
use orbit_common::protocol::tool_input::parse_duration_seconds;
use orbit_core::Core;
use orbit_core::WorkspacePullRequest;
use orbit_core::app::run::{Command, exec};
use orbit_core::application::distributed::PullStartOptionsSupplied;
use orbit_core::application::job::{AutoDrainSubmissionProfile, ShipSubmissionProfile};
use orbit_types::workflow::{JobRunTrigger, ReviewLayer};
use serde_json::{Value, json};

use crate::api::routines::{OperationsQuery, explicit_workspace};
use crate::api::{OptionalJson, bad_request, blocking, validate_id};

/// Minimum window this endpoint accepts for `for_duration`, in seconds. A
/// single pass is asked for explicitly with `one_pass`, so `0s` or a blank
/// duration is rejected rather than silently read as one.
const MIN_AUTO_DRAIN_SECONDS: u64 = 1;

/// Read-only eligible/waiting projection for backlog-discovery auto-drain,
/// same limit the CLI's `orbit run readiness` defaults to.
const AUTO_DRAIN_READINESS_LIMIT: usize = 50;

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

    /// Which pull-start options this body supplied, for the shared decision.
    fn pull_start_options(&self) -> PullStartOptionsSupplied {
        PullStartOptionsSupplied {
            pull: self.pull.is_some(),
            host: self.host.is_some(),
            auto_concurrency: self.auto_concurrency.is_some(),
            complete: self.complete,
            approve_proposed: self.approve_proposed,
            auto_fix: self.auto_fix,
            strict_worker_containment: self.strict_worker_containment,
            low_complexity_crews: self.low_complexity_crews.is_some(),
            medium_complexity_crews: self.medium_complexity_crews.is_some(),
            hard_complexity_crews: self.hard_complexity_crews.is_some(),
            xhard_complexity_crews: self.xhard_complexity_crews.is_some(),
            claim_token: self.claim_token.is_some(),
            review: self.review.is_some(),
        }
    }
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

#[derive(serde::Deserialize, Default)]
pub(in crate::api) struct AutoDrainReadinessQuery {
    #[serde(default)]
    concurrency: Option<u32>,
    /// Comma-separated task ids to explain instead of the bounded backlog
    /// listing, so a task detail gets its own answer wherever it ranks.
    #[serde(default)]
    task: Option<String>,
    /// Add one task's full readiness entry beside the bounded workspace list.
    #[serde(default)]
    also_task: Option<String>,
    /// Maximum tasks to explain, including an explicit selection (1-500).
    #[serde(default)]
    limit: Option<usize>,
    /// Comma-separated configured crews to evaluate.
    #[serde(default)]
    allow_crew: Option<String>,
}

/// Submit a `ship` workflow run (`POST /workflows/ship?workspace=<id>`).
///
/// One-shot: responds as soon as the run is persisted, with the same
/// `queued`/`submitted` states the CLI reports, and the same submission
/// notices (`warning`, `validation_env_warning`) `orbit run ship` and
/// `orbit.workflow` action `ship` report. Callers poll `GET /runs/:id` for progress.
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
    WsCore(core): WsCore,
    OptionalJson(body): OptionalJson<ShipBody>,
) -> Response {
    let explicit = match body.mode.as_deref() {
        Some(raw) => match orbit_core::ShipMode::parse(raw) {
            Ok(mode) => Some(mode),
            Err(e) => return bad_request(e.to_string()),
        },
        None => None,
    };
    match blocking("ship workflow", move || {
        let receipt = exec(
            &core,
            Command::ship(
                explicit,
                orbit_core::ShipRunRequest {
                    base_branch: body.base.as_deref(),
                    task_ids: &body.task_ids,
                    // [ORB-11187] Completion authority is granted per invocation at the CLI
                    // (`orbit run ship --complete`); the dashboard does not offer it, so
                    // this endpoint always ends successful work at `review`.
                    completion: orbit_core::CompletionPolicy::Review,
                    allowed_crews: &[],
                    actor: Some("dashboard"),
                    claim_token: body.claim_token.as_deref(),
                    trigger: JobRunTrigger::dashboard(),
                    // Strict worker containment is a CLI-only flag.
                    strict_containment: false,
                    review: body.review,
                    // The dashboard's Ship is an operator's explicit ship: it
                    // overrides the drain's readiness gates for the named tasks.
                    operator_override: true,
                    ignore_dependency_delivery: false,
                    ..Default::default()
                },
                ShipSubmissionProfile::Http,
            ),
        )?
        .value;
        Ok(receipt.to_json())
    })
    .await
    {
        Ok(shipped) => Json(shipped).into_response(),
        Err(response) => *response,
    }
}

fn explicit_auto_concurrency<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<orbit_types::workflow::AutoConcurrencyMode>, D::Error> {
    <orbit_types::workflow::AutoConcurrencyMode as serde::Deserialize>::deserialize(deserializer)
        .map(Some)
}

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
/// `validation_env_warning`) as `orbit.workflow` action `auto` does, and never the
/// claim token.
pub(in crate::api) async fn auto_drain_workflow_action(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    WsCore(core): WsCore,
    OptionalJson(body): OptionalJson<AutoDrainBody>,
) -> Response {
    if let Err(rejection) = explicit_workspace(&query) {
        return rejection.into_response();
    }
    let for_seconds = match drain_window_seconds(&body) {
        Ok(seconds) => seconds,
        Err(message) => return bad_request(message),
    };
    if let Some(refusal) = body.pull_start_options().pull_start_refusal() {
        return bad_request(refusal);
    }
    if body.pull.is_some() {
        let global_root = state.global_root().to_path_buf();
        return start_blocking("pull-drain workflow", move || {
            pull_drain_start(&core, &global_root, &body, for_seconds)
        })
        .await;
    }
    match helpers::blocking_join("auto-drain workflow", move || {
        let completion = if body.complete {
            orbit_core::CompletionPolicy::Done
        } else {
            orbit_core::CompletionPolicy::Review
        };
        exec(
            &core,
            Command::dashboard_auto(orbit_core::AutoDrainRequest {
                for_seconds,
                max_active_leaf_runs: body.concurrency,
                completion,
                allowed_crews: &body.allow_crew,
                complexity_crews: body.complexity_crews(),
                actor: Some("dashboard"),
                claim_token: body.claim_token.as_deref(),
                trigger: JobRunTrigger::dashboard(),
                strict_containment: body.strict_worker_containment,
                approve_proposed: body.approve_proposed,
                auto_fix: body.auto_fix,
                auto_concurrency: body.auto_concurrency,
                review: body.review,
            }),
        )
        .map(|outcome| outcome.value)
    })
    .await
    {
        Ok(Ok(receipt)) => Json(receipt.to_json()).into_response(),
        Ok(Err(error)) => map_start_refusal(error),
        Err(response) => *response,
    }
}

/// Run a start on the blocking pool, answering with its receipt or with the
/// refusal [`map_start_refusal`] words.
async fn start_blocking<F>(label: &'static str, op: F) -> Response
where
    F: FnOnce() -> Result<Value, orbit_core::OrbitError> + Send + 'static,
{
    match helpers::blocking_join(label, op).await {
        Ok(Ok(started)) => Json(started).into_response(),
        Ok(Err(error)) => map_start_refusal(error),
        Err(response) => *response,
    }
}

/// `orbit run auto --pull`: resolve the owner selector as the CLI does, then
/// submit through the replica's pull path. The receipt names the selector the
/// window pulls from and, like the CLI, warns when this replica's validation
/// gates drifted from the owner's.
fn pull_drain_start(
    core: &Core,
    global_root: &std::path::Path,
    body: &AutoDrainBody,
    for_seconds: Option<u64>,
) -> Result<Value, orbit_core::OrbitError> {
    let pull = body.pull.as_deref().unwrap_or_default();
    let selector = orbit_cmd::hosts::pull_selector(global_root, body.host.as_deref(), pull)?;
    let receipt = exec(
        core,
        Command::pull(
            WorkspacePullRequest {
                selector: &selector,
                for_seconds,
                max_active_leaf_runs: body.concurrency,
                allowed_crews: &body.allow_crew,
                actor: Some("dashboard"),
            },
            JobRunTrigger::dashboard(),
            AutoDrainSubmissionProfile::Http,
        ),
    )?
    .value;
    Ok(receipt.to_json())
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
    Query(query): Query<OperationsQuery>,
    WsCore(core): WsCore,
    body: Result<OptionalJson<AutoDrainStopBody>, Response>,
) -> Response {
    if let Err(rejection) = explicit_workspace(&query) {
        return rejection.into_response();
    }
    match blocking("auto-drain stop", move || {
        exec(
            &core,
            Command::dashboard_stop(
                body.map(|OptionalJson(body)| (body.reason, body.claim_token))
                    .map_err(Box::new),
            ),
        )
        .map(|outcome| outcome.value)
    })
    .await
    {
        Ok(Err(response)) => *response,
        Ok(Ok(result)) => {
            let mut stopped = result.to_json();
            stopped["workflow"] = json!("auto");
            Json(stopped).into_response()
        }
        Err(response) => *response,
    }
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

/// `GET /workflows/auto/readiness?workspace=<id>[&concurrency=N][&task=<id>,…][&also_task=<id>][&limit=N][&allow_crew=<crew>,…]`
/// — the read-only eligible/waiting snapshot for backlog-discovery
/// auto-drain, projected straight from `workspace_auto_readiness`
/// (the same snapshot `orbit run readiness` prints) rather than a
/// dashboard-local recomputation of eligibility. `task` explains exactly
/// those tasks, as `orbit run readiness <id>…` does; an unknown id is a 400.
/// `also_task` adds one full entry beside the bounded list, projected from
/// that same admission snapshot rather than a second read of it. If that task
/// has disappeared from the workspace, the response keeps the bounded snapshot
/// and reports its id in `detail_unavailable`.
pub(in crate::api) async fn auto_drain_readiness(
    State(state): State<DashboardState>,
    WsCore(core): WsCore,
    Query(query): Query<AutoDrainReadinessQuery>,
) -> Response {
    let task_ids = match readiness_task_ids(query.task.as_deref()) {
        Ok(ids) => ids,
        Err(message) => return bad_request(message),
    };
    let also_task_ids = match readiness_task_ids(query.also_task.as_deref()) {
        Ok(ids) if ids.len() <= 1 => ids,
        Ok(_) => return bad_request("also_task accepts one task id".to_string()),
        Err(message) => return bad_request(message),
    };
    let allowed_crews = query
        .allow_crew
        .as_deref()
        .map(orbit_common::protocol::tool_input::split_csv)
        .unwrap_or_default();
    let hosts = state.hosts().snapshot;
    match blocking("auto-drain readiness", move || {
        exec(
            &core,
            Command::dashboard_readiness(
                &task_ids,
                also_task_ids.first().map(String::as_str),
                query.concurrency,
                query.limit.unwrap_or(AUTO_DRAIN_READINESS_LIMIT),
                &allowed_crews,
                hosts.as_deref().map(|snapshot| &snapshot.registry),
            ),
        )
        .map(|outcome| outcome.value)
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
            Json(payload).into_response()
        }
        Err(response) => *response,
    }
}
