//! Routine and machine clock operations for the dashboard [ORB-10875].

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Local, Utc};
use orbit_cmd::registry_routines::{
    RoutineStoreFingerprint, routine_status_input_paths, routine_status_store_fingerprint,
    routine_status_task_store_paths, routine_statuses,
};
use orbit_common::Env;
use orbit_common::governance::authorization::{
    AuthorizationDenial, DASHBOARD_CLOCK_CADENCE, DASHBOARD_CLOCK_SERVICE, DASHBOARD_JOB_RUN,
    DASHBOARD_ROUTINE_BREAKER, DASHBOARD_ROUTINE_TOGGLE, DASHBOARD_ROUTINE_TRIGGER,
    GovernedOperation,
};
use orbit_core::Core;
use orbit_core::app::DashboardOrigin;
pub(super) use orbit_core::app::dashboard_authorize as authorized_caller;
use orbit_core::app::routine::{
    self, ClockAction, ClockControl, ClockControlled, Command, DashboardWrite, DashboardWritten,
    ListedRoutine, Outcome,
};
use orbit_core::application::routines::{
    ClockHealth, ClockStatus, FireField, RoutineEditRefusal, RoutineField, RoutineStatus,
    RoutineStatusReport, trigger_options,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{blocking, map_runtime_error};
// Compatibility shim. Retirement: callers import `crate::runtime_memo::RoutineMemoKey`.
pub(crate) use crate::runtime_memo::RoutineMemoKey;
use crate::state::{DashboardState, WsCore};

/// Freshness bound of a served routine health body. Sampled fires, pauses and
/// definition or config edits change the memo key and recompute at once. Job
/// runs and the task store move with every run a live drain starts or
/// finishes, so they only mark the body for one background recompute
/// ([`RoutineChurn`]) and it is served for up to this bound meanwhile. The
/// bound also covers what the key cannot see (the clock, due times, automation
/// state) and in-place updates outside the newest-256 sampled fire/job-run rows.
const ROUTINE_HEALTH_TTL: Duration = Duration::from_secs(5);

/// Age at which a still-fresh hit starts a background recompute, leaving the
/// rest of [`ROUTINE_HEALTH_TTL`] for it to land before the entry expires.
/// A request past the bound, or whose refresh failed or lands late, waits for
/// compute.
const ROUTINE_HEALTH_REFRESH_AFTER: Duration = Duration::from_secs(3);

const FIRE_COLUMNS: &[(&str, FireField)] = &[
    ("slot", FireField::Slot),
    ("attempt", FireField::Attempt),
    ("state", FireField::State),
    ("ok", FireField::Ok),
    ("run_id", FireField::RunId),
    ("detail", FireField::Detail),
    ("started_at", FireField::StartedAt),
    ("finished_at", FireField::FinishedAt),
    ("duration_ms", FireField::DurationMs),
];

/// A routine row's columns; `automation` is summarized on render.
const ROUTINE_COLUMNS: &[(&str, RoutineField)] = &[
    ("name", RoutineField::Name),
    ("orbit_default", RoutineField::OrbitDefault),
    ("description", RoutineField::Description),
    ("source", RoutineField::Source),
    ("origin", RoutineField::Origin),
    ("target", RoutineField::Target),
    ("enabled", RoutineField::Enabled),
    ("paused_at", RoutineField::PausedAt),
    ("effective", RoutineField::Effective),
    ("cron", RoutineField::Cron),
    ("trigger", RoutineField::Trigger),
    ("automation", RoutineField::Automation),
    ("first_observed_at", RoutineField::FirstObservedAt),
    ("last_evaluated_slot", RoutineField::LastEvaluatedSlot),
    ("next_due", RoutineField::NextDue),
    ("next_evaluation", RoutineField::NextEvaluation),
    ("last_fire", RoutineField::LastFire(FIRE_COLUMNS)),
    ("recent_fires", RoutineField::RecentFires(FIRE_COLUMNS)),
    ("failure_streak", RoutineField::FailureStreak),
    ("state", RoutineField::State),
    ("breaker", RoutineField::Breaker),
];

/// The error a clock control answers with when the clock changed while it was pending.
const CLOCK_CHANGED_ERROR: &str =
    "clock state changed while this action was pending; refresh before retrying";

/// Inputs routine health reads that a live drain rewrites constantly: job runs
/// and the task registry files. Keying on them recomputed synchronously, about
/// 1-2 s each, whenever any run anywhere started or finished; a change instead
/// serves the previous body, at most [`ROUTINE_HEALTH_TTL`] old, while one
/// background recompute replaces it.
#[derive(Hash)]
struct RoutineChurn {
    job_runs: Option<String>,
    task_store: Vec<(PathBuf, Option<(SystemTime, u64)>)>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct OperationsQuery {
    pub(super) workspace: Option<String>,
    /// List reads only: also return definitions seeded by a plugin that is
    /// off where they live, marked `plugin_inactive`. Hidden by default.
    #[serde(default)]
    pub(super) include_inactive_plugins: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct RoutineToggleRequest {
    name: String,
    source: String,
    target: String,
    /// The machine the browser acted from. Recorded in the audit event;
    /// routine definitions are machine-independent [ORB-12236], so it selects
    /// nothing.
    machine_name: String,
    expected_enabled: bool,
    enabled: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct ClockControlRequest {
    action: ClockAction,
    machine_name: String,
    expected_enabled: bool,
    expected_cadence_seconds: u64,
    cadence_seconds: Option<u64>,
}

/// Record `path`'s modification time and length, recursing into directories.
/// Best effort: an unreadable input stamps as absent rather than failing the
/// read, and the memo's short TTL still bounds how long a change can hide.
fn stamp(path: &Path, inputs: &mut Vec<(PathBuf, Option<(SystemTime, u64)>)>) {
    let metadata = std::fs::symlink_metadata(path).ok();
    inputs.push((
        path.into(),
        metadata
            .as_ref()
            .and_then(|metadata| Some((metadata.modified().ok()?, metadata.len()))),
    ));
    if metadata.is_some_and(|metadata| metadata.is_dir())
        && let Ok(entries) = std::fs::read_dir(path)
    {
        for entry in entries.flatten() {
            stamp(&entry.path(), inputs);
        }
    }
}

/// `GET /api/routines` — routine definition state and the independent host clock.
///
/// Clock inspection is independent of definition load: a native-manager
/// transport failure still returns routine rows, with the clock projected as
/// `health: unknown` rather than HTTP 500.
///
/// A routine seeded by a plugin that is off where it lives is omitted unless
/// `include_inactive_plugins` is set; `inactive_plugin_counts` always reports
/// how many each workspace hides.
pub(super) async fn list_routine_health(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
) -> Response {
    let include_inactive_plugins = query.include_inactive_plugins;
    let operator_session = state.operator_session();
    let stamp_state = state.clone();
    let (key, churn) = match blocking("routine health inputs", move || {
        let mut inputs = Vec::new();
        for path in routine_status_input_paths(stamp_state.global_root())? {
            stamp(&path, &mut inputs);
        }
        inputs.sort();
        let mut task_store = Vec::new();
        for path in routine_status_task_store_paths(stamp_state.global_root()) {
            stamp(&path, &mut task_store);
        }
        let (routine_store, job_runs) =
            match routine_status_store_fingerprint(stamp_state.global_root()) {
                Some(RoutineStoreFingerprint { routines, job_runs }) => {
                    (Some(routines), Some(job_runs))
                }
                None => (None, None),
            };
        Ok((
            RoutineMemoKey::from_stamped_inputs(
                inputs,
                routine_store,
                include_inactive_plugins,
                operator_session,
            ),
            RoutineChurn {
                job_runs,
                task_store,
            },
        ))
    })
    .await
    {
        Ok(stamped) => stamped,
        Err(response) => return *response,
    };
    let compute_state = state.clone();
    match state
        .routines_memo()
        .get_or_compute_for_host_ahead(
            key,
            &churn,
            ROUTINE_HEALTH_TTL,
            ROUTINE_HEALTH_REFRESH_AFTER,
            move || {
                compute_routine_health(
                    compute_state,
                    Utc::now(),
                    operator_session,
                    include_inactive_plugins,
                )
            },
        )
        .await
    {
        Ok(served) => (served.age_header(), Json((*served.body).clone())).into_response(),
        Err(error) => map_runtime_error(error),
    }
}

// Called exclusively by RuntimeMemo's spawn_blocking computation.
fn compute_routine_health(
    state: DashboardState,
    generated_at: DateTime<Utc>,
    operator_session: bool,
    include_inactive_plugins: bool,
) -> Result<Value, orbit_core::OrbitError> {
    let started = Instant::now();
    let report = routine_statuses(state.global_root())?;
    let report_ms = started.elapsed().as_millis();
    let clock = match state.clock_status() {
        Ok(status) => clock_json(&status),
        Err(error) => unavailable_clock_json(&error.to_string()),
    };
    let clock_ms = started.elapsed().as_millis();
    let body = report_json(
        &report,
        clock,
        generated_at,
        operator_session,
        include_inactive_plugins,
        state.env(),
    );
    tracing::debug!(
        report_ms,
        clock_ms,
        total_ms = started.elapsed().as_millis(),
        "routine health computed"
    );
    Ok(body)
}

/// The authenticated session a governed write arrived on, as the app that
/// runs it records it: the serving host, the workspace selector the request
/// addressed and the machine the browser acted from.
pub(super) fn dashboard_caller(
    state: &DashboardState,
    workspace: Option<&str>,
    machine_name: &str,
) -> DashboardOrigin {
    DashboardOrigin {
        host: state
            .env()
            .hostname()
            .and_then(OsStr::to_str)
            .map(str::to_string),
        workspace: workspace.map(str::to_string),
        machine_name: machine_name.to_string(),
    }
}

/// An app answered one command with another command's outcome.
pub(super) fn unexpected_outcome(label: &str) -> Response {
    super::server_error(orbit_core::OrbitError::Execution(format!(
        "{label} answered with another command's outcome"
    )))
}

/// The host's routine status read a routine command runs once its caller is
/// admitted.
pub(super) fn statuses_read(state: &DashboardState) -> routine::Statuses {
    let global_root = state.global_root().to_path_buf();
    Box::new(move || routine_statuses(&global_root))
}

/// The sweep clock's state and the routine status report a clock control
/// reads before it changes anything. `routine_write` runs it on the blocking
/// pool once the caller is admitted.
fn clock_observation(state: &DashboardState) -> routine::ClockObservation {
    let state = state.clone();
    Box::new(move || {
        let before = state.clock_status()?;
        let report = routine_statuses(state.global_root())?;
        Ok((before, report))
    })
}

/// Run one governed routine write on the blocking pool and return what the
/// admitted write did, or the response that already answers the request.
pub(super) async fn routine_write(
    label: &'static str,
    core: Arc<Core>,
    caller: DashboardOrigin,
    write: DashboardWrite,
) -> Result<DashboardWritten, Box<Response>> {
    match blocking(label, move || {
        routine::exec(&core, Command::Dashboard(caller, write))
    })
    .await?
    {
        Outcome::Dashboard(written) => Ok(written),
        Outcome::EnabledSet(_) => Err(Box::new(unexpected_outcome(label))),
    }
}

/// `POST /api/routines/toggle` — atomically change one selected workspace's
/// versioned `enabled` field. Browser input never supplies a path.
///
/// The routine app authorizes, edits and audits: an unlisted routine is a
/// 404 with nothing to audit, and every other refusal (owner authority,
/// workspace, stale state, retargeted definition) wrote nothing, is audited
/// and answers 409.
pub(super) async fn toggle_routine(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    WsCore(core): WsCore,
    Json(body): Json<RoutineToggleRequest>,
) -> Response {
    let workspace = match explicit_workspace(&query) {
        Ok(workspace) => workspace,
        Err(rejection) => return rejection.into_response(),
    };
    let written = routine_write(
        "routine toggle",
        core,
        dashboard_caller(&state, Some(workspace), &body.machine_name),
        DashboardWrite::Toggle {
            routine: ListedRoutine {
                name: body.name.clone(),
                source: body.source.clone(),
                target: body.target.clone(),
            },
            expected_enabled: body.expected_enabled,
            enabled: body.enabled,
            statuses: statuses_read(&state),
        },
    )
    .await;
    let outcome = match written {
        Ok(DashboardWritten::Edited(Ok(Ok(outcome)))) => outcome,
        Ok(DashboardWritten::Edited(Ok(Err(refusal)))) => return routine_edit_refused(&refusal),
        Ok(DashboardWritten::Edited(Err(error))) => return map_runtime_error(error),
        Ok(_) => return unexpected_outcome("routine toggle"),
        Err(response) => return *response,
    };
    Json(json!({
        "name": body.name,
        "source": body.source,
        "target": body.target,
        "machine_name": body.machine_name,
        "enabled": body.enabled,
        "changed": outcome.changed,
        "message": if body.enabled { "Routine enabled" } else { "Routine disabled" },
    }))
    .into_response()
}

/// A refused routine definition edit as the dashboard answers it: the core
/// refusal body, 404 for an unlisted routine and 409 (resolved by refreshing)
/// for every other refusal.
pub(super) fn routine_edit_refused(refusal: &RoutineEditRefusal) -> Response {
    let status = match refusal {
        RoutineEditRefusal::NotFound { .. } => StatusCode::NOT_FOUND,
        _ => StatusCode::CONFLICT,
    };
    (status, Json(refusal.to_json())).into_response()
}

/// `POST /api/routines/clock` — typed native-service or cadence control.
///
/// The routine app authorizes the control, checks it against the machine
/// and clock state the browser observed, applies it and audits an applied
/// change.
pub(super) async fn control_clock(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    WsCore(core): WsCore,
    Json(body): Json<ClockControlRequest>,
) -> Response {
    let workspace = match explicit_workspace(&query) {
        Ok(workspace) => workspace,
        Err(rejection) => return rejection.into_response(),
    };
    let observe = clock_observation(&state);
    #[cfg(test)]
    let stand_in: Option<routine::ClockStandIn> = {
        let state = state.clone();
        Some(Box::new(move || state.test_clock_mutation()))
    };
    #[cfg(not(test))]
    let stand_in = None;
    let written = routine_write(
        "clock control",
        core,
        dashboard_caller(&state, Some(workspace), &body.machine_name),
        DashboardWrite::Clock(ClockControl {
            action: body.action,
            cadence_seconds: body.cadence_seconds,
            expected_enabled: body.expected_enabled,
            expected_cadence_seconds: body.expected_cadence_seconds,
            global_root: state.global_root().to_path_buf(),
            observe,
            stand_in,
        }),
    )
    .await;
    let before = match written {
        Ok(DashboardWritten::Clock(ClockControlled::MachineMismatch { machine_name })) => {
            return selection_conflict(
                "machine_mismatch",
                format!("select machine '{machine_name}' before changing its clock"),
            );
        }
        Ok(DashboardWritten::Clock(ClockControlled::Stale(before))) => {
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": CLOCK_CHANGED_ERROR,
                    "code": "stale_clock_state",
                    "actual_enabled": before.enabled,
                    "actual_cadence_seconds": before.configured_cadence_seconds,
                })),
            )
                .into_response();
        }
        Ok(DashboardWritten::Clock(ClockControlled::Applied { before, result })) => {
            if let Err(error) = result {
                return map_runtime_error(error);
            }
            *before
        }
        Ok(_) => return unexpected_outcome("clock control"),
        Err(response) => return *response,
    };
    let state_after = state.clone();
    let after = tokio::task::spawn_blocking(move || state_after.clock_status()).await;
    let (clock, changed) = match after {
        Ok(Ok(status)) => (clock_json(&status), json!(before != status)),
        Ok(Err(error)) => (unavailable_clock_json(&error.to_string()), Value::Null),
        Err(error) => (
            unavailable_clock_json(&format!("clock status after panicked: {error}")),
            Value::Null,
        ),
    };
    Json(json!({
        "clock": clock,
        "changed": changed,
        "message": match body.action {
            ClockAction::Enable => "Sweep clock enabled",
            ClockAction::Disable => "Sweep clock paused",
            ClockAction::SetCadence => "Sweep clock cadence updated",
        },
    }))
    .into_response()
}

pub(super) fn report_json(
    report: &RoutineStatusReport,
    clock: Value,
    generated_at: DateTime<Utc>,
    operator_session: bool,
    include_inactive_plugins: bool,
    env: &Env,
) -> Value {
    let mut inactive_plugin_counts = BTreeMap::<&str, usize>::new();
    for routine in report.inactive_plugin_routines() {
        *inactive_plugin_counts
            .entry(routine.source_workspace.as_str())
            .or_default() += 1;
    }
    json!({
        "generated_at": generated_at.to_rfc3339(),
        "machine_name": report.machine_name,
        "machine_id": report.machine_id,
        "controls_authorized": authorized_caller(&DASHBOARD_ROUTINE_TOGGLE, operator_session).is_ok(),
        "capabilities": {
            "routine_toggle": action_capability(&DASHBOARD_ROUTINE_TOGGLE, operator_session),
            "routine_trigger": action_capability(&DASHBOARD_ROUTINE_TRIGGER, operator_session),
            "routine_breaker": action_capability(&DASHBOARD_ROUTINE_BREAKER, operator_session),
            "job_run": action_capability(&DASHBOARD_JOB_RUN, operator_session),
            "clock_service": action_capability(&DASHBOARD_CLOCK_SERVICE, operator_session),
            "clock_cadence": action_capability(&DASHBOARD_CLOCK_CADENCE, operator_session),
        },
        "session_explanation": if authorized_caller(&DASHBOARD_ROUTINE_TOGGLE, operator_session).is_ok() {
            "Session access: this dashboard server has operator authority. Actions also check workspace and machine selection. Mint creates a task without starting delivery; bounded-window submission has separate permissions."
        } else {
            "Session access comes from the dashboard server. For operator access, start it with `orbit web serve --operator` (or `orbit web connect`, which does that by default) and reload this page. Neither the server's terminal nor `ORBIT_OPERATOR` grants operator access. Bounded-window submission has separate permissions."
        },
        "clock": clock,
        "cron_zone": host_cron_zone(env),
        "routines": report.statuses.iter().map(status_json).collect::<Vec<_>>(),
        "retired": report.retired_rows(include_inactive_plugins),
        "owner_only": report.owner_only_rows(),
        "inactive_plugin_counts": inactive_plugin_counts,
        "load_errors": report.load_error_rows(),
    })
}

pub(super) fn status_json(status: &RoutineStatus) -> Value {
    let mut row = status.project(ROUTINE_COLUMNS);
    // The trigger kinds the edit form may offer without touching the target.
    row.insert(
        "trigger_options".into(),
        json!(trigger_options(&status.routine)),
    );
    if let Some(automation) = &status.automation {
        row.insert("automation".into(), super::automation::summary(automation));
    }
    Value::Object(row)
}

/// The zone cron triggers are evaluated in: the host's local zone, as the
/// routine and auto-task schedulers use. `name` is the IANA name when the
/// host exposes one (`TZ`, the `/etc/localtime` link, `/etc/timezone`), else
/// null; `offset_seconds` is the current offset from UTC, so a client can
/// still label the zone when the name is unknown.
pub(super) fn host_cron_zone(env: &Env) -> Value {
    json!({
        "name": host_zone_name(env),
        "offset_seconds": Local::now().offset().local_minus_utc(),
    })
}

fn host_zone_name(env: &Env) -> Option<String> {
    let tz = env.tz().and_then(OsStr::to_str);
    let link = std::fs::read_link("/etc/localtime").ok();
    let file = std::fs::read_to_string("/etc/timezone").ok();
    zone_name_from_sources(
        tz,
        link.as_deref().and_then(std::path::Path::to_str),
        file.as_deref(),
    )
}

fn zone_name_from_sources(
    tz_env: Option<&str>,
    localtime_link: Option<&str>,
    timezone_file: Option<&str>,
) -> Option<String> {
    let plausible = |name: &str| {
        !name.is_empty()
            && name.len() <= 64
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '+'))
    };
    let from_env = tz_env.map(|tz| tz.trim().trim_start_matches(':'));
    let from_link = localtime_link
        .and_then(|target| target.split_once("zoneinfo/"))
        .map(|(_, name)| name);
    let from_file = timezone_file.map(str::trim);
    [from_env, from_link, from_file]
        .into_iter()
        .flatten()
        .find(|name| plausible(name))
        .map(str::to_string)
}

pub(super) fn clock_json(clock: &ClockStatus) -> Value {
    json!({
        "provider": clock.platform,
        "configured_cadence_seconds": clock.configured_cadence_seconds,
        "effective_cadence_seconds": clock.effective_cadence_seconds,
        "enabled": clock.enabled,
        "loaded": clock.loaded,
        "running": clock.running,
        "schedulable": clock.schedulable,
        // A reported issue alone does not change the dashboard's token.
        "health": match clock.health() {
            ClockHealth::Healthy | ClockHealth::ScheduledWithIssue => "healthy",
            ClockHealth::Missed => "missed",
            ClockHealth::Paused | ClockHealth::PausedWithIssue => "paused",
            ClockHealth::RunningWhilePaused => "unhealthy",
        },
        "health_issue": clock.health_issue,
        "last_tick_at": clock.last_tick_at,
        "next_tick_at": clock.next_tick_at,
        "error": null,
    })
}

/// Clock inspection failed: no observed enabled/paused/missed authority.
pub(super) fn unavailable_clock_json(error: &str) -> Value {
    json!({
        "provider": Value::Null,
        "configured_cadence_seconds": Value::Null,
        "effective_cadence_seconds": Value::Null,
        "enabled": Value::Null,
        "loaded": Value::Null,
        "running": Value::Null,
        "schedulable": Value::Null,
        "health": "unknown",
        "health_issue": error,
        "last_tick_at": Value::Null,
        "next_tick_at": Value::Null,
        "error": error,
    })
}

pub(super) fn explicit_workspace(
    query: &OperationsQuery,
) -> Result<&str, (StatusCode, Json<Value>)> {
    query
        .workspace
        .as_deref()
        .filter(|workspace| !workspace.trim().is_empty())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "select one concrete workspace before using Operations controls",
                    "code": "workspace_required",
                })),
            )
        })
}

/// Project the same authorization decision enforced by the mutation endpoint.
pub(super) fn action_capability(
    operation: &'static GovernedOperation,
    operator_session: bool,
) -> Value {
    match authorized_caller(operation, operator_session) {
        Ok(_) => json!({"authorized": true, "reason": null}),
        Err(denial) => json!({
            "authorized": false,
            "reason": denial.to_string(),
        }),
    }
}

pub(super) fn authorization_denied(denial: AuthorizationDenial) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": denial.to_string(),
            "code": "authorization_denied",
            "operation": denial.operation.id,
            "provenance": denial.provenance.to_string(),
        })),
    )
        .into_response()
}

/// The named routine or auto-task does not exist: a permanent 404, so a
/// client's "409 means refresh and retry" policy does not loop on it.
pub(super) fn named_entity_not_found(code: &'static str, message: String) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error": message, "code": code})),
    )
        .into_response()
}

/// The caller's selection (workspace, host, target) no longer matches the
/// entity it is trying to change: a 409 the client resolves by refreshing.
pub(super) fn selection_conflict(code: &'static str, message: String) -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({"error": message, "code": code})),
    )
        .into_response()
}
