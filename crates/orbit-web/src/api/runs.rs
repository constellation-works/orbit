//! Run lifecycle: detail, cancel, replay, events, logs.

use crate::state::Ws;
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use orbit_common::governance::authorization::DASHBOARD_AUTO_DRAIN_COMPLETE;
use orbit_common::security::redaction::redact_all;
use orbit_core::application::job::{
    ActivityInvocationEvidence, job_run_to_json_with_activity_provenance,
};
use orbit_core::runtime::run_audit::{RunAuditStep, RunCliInvocationRecord, RunProviderProcess};
use orbit_core::{InvocationQuery, JobRun, OrbitRuntime, V2AuditEventFilter};
use serde_json::{Value, json};

use super::routines::{authorization_denied, authorized_caller};
use super::{
    HISTORY_DEFAULT_LIMIT, LimitQuery, RunEventsQuery, bad_request, blocking, bounded_limit,
    map_runtime_error, validate_id,
};

const RUN_EVENTS_DEFAULT_LIMIT: usize = 100;
/// Hard cap on rows scanned from a single run's persisted v2 audit events.
pub(super) const RUN_EVENTS_MAX_SCAN_LINES: usize = 50_000;
/// Capacity hint for the returned events page. A fixed constant rather than
/// the caller-supplied `limit` so the allocation size never derives from
/// request input, regardless of how large a `limit` a caller requests.
const RUN_EVENTS_PAGE_CAPACITY_HINT: usize = 64;
/// Maximum bytes included in stdout/stderr previews returned by run-log APIs.
const RUN_LOG_PREVIEW_MAX_BYTES: usize = 8192;
/// Maximum lines included in stdout/stderr previews returned by run-log APIs.
const RUN_LOG_PREVIEW_MAX_LINES: usize = 120;

#[derive(serde::Deserialize, Default)]
pub(super) struct ShipBody {
    /// Explicit task selection; empty selects auto (backlog-discovery) mode.
    #[serde(default)]
    task_ids: Vec<String>,
    /// `"pr"` or `"local"`. Omitted means the workspace's own configured ship
    /// mode (ORB-10444) — what the dashboard's one-click Ship sends.
    #[serde(default)]
    mode: Option<String>,
    /// Base branch override; defaults to the workspace's `[workflow] base_branch`.
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
/// `queued`/`submitted` states the CLI reports. Callers poll
/// `GET /runs/:id` for progress.
///
/// An omitted `mode` resolves to the selected workspace's own ship mode
/// (ORB-10444), so the dashboard's one-click Ship needs no PR/local toggle;
/// a workspace with no registry binding (single/embedded mode) keeps the
/// historical `pr` default.
///
/// [ORB-10544] Duplicate dispatch of an explicitly-selected task is refused by
/// the shared submission path, not here: `submit_ship_run` returns
/// `OrbitError::ShipRunInFlight` when one of the named tasks is already carried
/// by a non-terminal run, which `map_runtime_error` projects to this endpoint's
/// stable 409. Auto (backlog-discovery) mode has no task ids to guard and is
/// unaffected.
pub(super) async fn ship_workflow_action(
    Ws(runtime): Ws,
    body: Option<Json<ShipBody>>,
) -> Response {
    let Json(body) = body.unwrap_or_default();
    let mode = match body.mode.as_deref() {
        Some(raw) => match orbit_core::ShipMode::parse(raw) {
            Ok(mode) => mode,
            Err(e) => return bad_request(e.to_string()),
        },
        None => workspace_default_ship_mode(&runtime),
    };
    match blocking("ship workflow", move || {
        runtime.submit_ship_run(
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
        )
    })
    .await
    {
        Ok(invoke) => Json(json!({
            "workflow": "ship",
            "job_id": invoke.job_name,
            "run_id": invoke.run_id,
            "state": if invoke.queued { "queued" } else { "submitted" },
            "submitted_at": invoke.submitted_at,
        }))
        .into_response(),
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
pub(super) struct AutoDrainBody {
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
/// does. `orbit-web` does not depend on `orbit-cli` (the dependency edge runs
/// the other way), so this ~20-line parser is duplicated here rather than
/// shared across that boundary.
fn parse_drain_duration_seconds(raw: &str) -> Result<u64, String> {
    let value = raw.trim();
    if value.is_empty() {
        return Err("for_duration must not be empty".to_string());
    }
    let split_at = value
        .find(|c: char| c.is_alphabetic())
        .ok_or_else(|| format!("invalid duration: {raw}"))?;
    let (num_raw, unit_raw) = value.split_at(split_at);
    let num: u64 = num_raw
        .parse()
        .map_err(|_| format!("invalid duration number: {raw}"))?;
    let seconds = match unit_raw {
        "s" => Some(num),
        "m" => num.checked_mul(60),
        "h" => num.checked_mul(3600),
        "d" => num.checked_mul(86400),
        "w" => num.checked_mul(604800),
        _ => {
            return Err(format!(
                "invalid duration unit: {unit_raw} (expected s/m/h/d/w)"
            ));
        }
    }
    .ok_or_else(|| format!("duration '{raw}' is too large to represent"))?;
    if seconds < MIN_AUTO_DRAIN_SECONDS {
        return Err("for_duration must describe a bounded window greater than zero".to_string());
    }
    Ok(seconds)
}

/// Submit a bounded `auto` workflow run
/// (`POST /workflows/auto?workspace=<id>`).
///
/// Dashboard counterpart to `orbit run auto --for <duration> [--concurrency
/// N] [--complete]`, reusing the same `submit_workspace_auto_run` runtime
/// path: a concrete workspace only (the `Ws` extractor refuses all-workspace
/// mode the same way `ship_workflow_action` does, and `submit_workspace_auto_run`
/// enforces the workspace claim), a required bounded duration, and an
/// explicit, separately-governed opt-in to `CompletionPolicy::Done` — opting
/// in authorizes `review -> done` for every task the window ships, not only
/// the ones visible now, so it is gated the same way `auto_task.mint`'s
/// unconditional mint is.
pub(super) async fn auto_drain_workflow_action(
    Ws(runtime): Ws,
    body: Option<Json<AutoDrainBody>>,
) -> Response {
    let Json(body) = body.unwrap_or_default();
    let for_seconds = match parse_drain_duration_seconds(&body.for_duration) {
        Ok(seconds) => seconds,
        Err(message) => return bad_request(message),
    };
    let completion = if body.complete {
        match authorized_caller(&DASHBOARD_AUTO_DRAIN_COMPLETE) {
            Ok(_) => orbit_core::CompletionPolicy::Done,
            Err(denial) => return authorization_denied(denial),
        }
    } else {
        orbit_core::CompletionPolicy::Review
    };
    match blocking("auto-drain workflow", move || {
        runtime.submit_workspace_auto_run(
            Some(for_seconds),
            body.concurrency,
            completion,
            // [ORB-11242] The dashboard launch form does not offer a crew
            // restriction, so it submits the unrestricted window it always has.
            &[],
            &Default::default(),
            Some("dashboard"),
            body.claim_token.as_deref(),
        )
    })
    .await
    {
        Ok(invoke) => Json(json!({
            "workflow": "auto",
            "job_id": invoke.job_name,
            "run_id": invoke.run_id,
            "state": if invoke.queued { "queued" } else { "submitted" },
            "submitted_at": invoke.submitted_at,
            "completion": completion.as_input_value(),
        }))
        .into_response(),
        Err(response) => *response,
    }
}

#[derive(serde::Deserialize, Default)]
pub(super) struct AutoDrainReadinessQuery {
    #[serde(default)]
    concurrency: Option<u32>,
}

/// `GET /workflows/auto/readiness?workspace=<id>[&concurrency=N]` — the
/// read-only eligible/waiting snapshot for backlog-discovery auto-drain,
/// projected straight from `OrbitRuntime::workspace_auto_readiness` (the same
/// snapshot `orbit run readiness` prints) rather than a dashboard-local
/// recomputation of eligibility.
pub(super) async fn auto_drain_readiness(
    Ws(runtime): Ws,
    Query(query): Query<AutoDrainReadinessQuery>,
) -> Response {
    match runtime.workspace_auto_readiness(&[], query.concurrency, AUTO_DRAIN_READINESS_LIMIT, &[])
    {
        Ok(mut payload) => {
            // So the form can hide/disable the `complete` opt-in before the
            // operator ever hits the separately-governed 403 at submission.
            if let Some(object) = payload.as_object_mut() {
                object.insert(
                    "controls_authorized".to_string(),
                    Value::Bool(authorized_caller(&DASHBOARD_AUTO_DRAIN_COMPLETE).is_ok()),
                );
            }
            Json(payload).into_response()
        }
        Err(e) => map_runtime_error(e),
    }
}

pub(super) async fn get_run(Ws(runtime): Ws, Path(id): Path<String>) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id.to_string(),
        Err(message) => return bad_request(message),
    };
    match blocking("get run", move || {
        let run = runtime.show_job_run(&id)?;
        Ok(job_run_detail_to_json(&runtime, &run))
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

pub(super) async fn cancel_run_action(Ws(runtime): Ws, Path(id): Path<String>) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id.to_string(),
        Err(message) => return bad_request(message),
    };
    match blocking("cancel run", move || {
        Ok(runtime.cancel_job_run_with_context(&id, "dashboard", "web"))
    })
    .await
    {
        Ok(Ok(result)) => Json(json!({
            "run_id": result.run_id,
            "outcome": result.outcome,
            "previous_state": result.previous_state,
            "final_state": result.final_state,
            "actor": result.actor,
            "source": result.source,
            "signal_attempted": result.signal_attempted,
            "signal_outcome": result.signal_outcome,
        }))
        .into_response(),
        Ok(Err(orbit_core::OrbitError::JobValidation(msg)))
        | Ok(Err(orbit_core::OrbitError::JobRunStateTransition(msg))) => {
            (StatusCode::CONFLICT, Json(json!({ "error": msg }))).into_response()
        }
        Ok(Err(e)) => map_runtime_error(e),
        Err(response) => *response,
    }
}

pub(super) async fn replay_run_action(Ws(runtime): Ws, Path(id): Path<String>) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id.to_string(),
        Err(message) => return bad_request(message),
    };
    match blocking("replay run", move || runtime.replay_job_run(&id)).await {
        Ok(result) => Json(json!({ "run_id": result.run_id })).into_response(),
        Err(response) => *response,
    }
}

pub(super) fn job_run_detail_to_json(runtime: &OrbitRuntime, run: &JobRun) -> Value {
    // [ORB-10971] Read the run's pipeline state. This projection used to drop
    // it entirely, so the dashboard could not see the waiting reasons or the
    // child-dispatch lineage the CLI already showed.
    let state = runtime.read_run_state(&run.run_id).ok().flatten();
    let evidence = runtime
        .invocation_records(InvocationQuery {
            job_run_id: Some(run.run_id.clone()),
            limit: 1_000,
            ..InvocationQuery::default()
        })
        .unwrap_or_default()
        .into_iter()
        .map(|record| ActivityInvocationEvidence {
            activity_id: record.activity_id,
            provider: record.agent,
            model: record.model,
        })
        .collect::<Vec<_>>();
    let mut full = job_run_to_json_with_activity_provenance(run, state.as_ref(), &evidence);
    // Reshape into `{run, steps}` per the dashboard contract: peel the
    // `steps` array off the flat `job_run_to_json` output.
    let stored_steps = full
        .as_object_mut()
        .and_then(|m| m.remove("steps"))
        .unwrap_or(Value::Array(Vec::new()));

    let audit_steps = runtime
        .collect_run_audit_steps(&run.run_id)
        .unwrap_or_default();
    let steps = if audit_steps.is_empty() {
        stored_steps
    } else {
        Value::Array(
            audit_steps
                .iter()
                .map(|step| audit_step_to_json(step, run.state))
                .collect(),
        )
    };

    // [ORB-10496] Provider subprocesses for this run's agent steps, with a
    // liveness verdict for any that have not reported an exit. Without this a
    // healthy long-running ship-pipeline implementation agent is
    // indistinguishable from a dead child without shell access to the host.
    let provider_processes = runtime
        .collect_run_provider_processes(&run.run_id)
        .unwrap_or_default();

    json!({
        "run": full,
        "steps": steps,
        "provider_processes": provider_processes
            .iter()
            .map(run_provider_process_to_json)
            .collect::<Vec<_>>(),
    })
}

fn run_provider_process_to_json(record: &RunProviderProcess) -> Value {
    json!({
        "run_id": record.run_id,
        "event_id": record.event_id,
        "ts": record.ts.map(|ts| ts.to_rfc3339()),
        "step_id": record.step_id,
        "step_index": record.step_index,
        "provider": record.provider,
        "pid": record.pid,
        "pid_start_time": record.pid_start_time,
        "finished": record.finished,
        "liveness": record.liveness.as_str(),
        "exit_code": record.exit_code,
        "timed_out": record.timed_out,
        "duration_ms": record.duration_ms,
    })
}

/// Project one audit-derived step, reconciled against the run's own state.
///
/// [ORB-10971] Steps are rebuilt from `step_started` / `step_finished` audit
/// events, so a run killed mid-step leaves a `step_started` with no partner
/// and the step renders `running` forever. A cancelled parent blocked in a
/// dispatch wait is exactly that case. A step cannot outlive its run: once the
/// run is terminal, an unfinished step inherits the run's terminal state and is
/// marked `interrupted` so the projection says what happened rather than
/// implying work is still in flight.
fn audit_step_to_json(step: &RunAuditStep, run_state: orbit_core::JobRunState) -> Value {
    let duration_ms = match (step.started_at, step.finished_at) {
        (Some(started), Some(finished)) => Some(
            finished
                .signed_duration_since(started)
                .num_milliseconds()
                .max(0) as u64,
        ),
        _ => None,
    };
    let unfinished = step.state.is_none();
    let state = match step.state.as_deref() {
        Some(state) => state.to_string(),
        None if run_state.is_terminal() => run_state.to_string(),
        None => "running".to_string(),
    };
    let outcome = match (&step.outcome, unfinished && run_state.is_terminal()) {
        (Some(outcome), _) => Some(outcome.clone()),
        (None, true) => Some("interrupted".to_string()),
        (None, false) => None,
    };

    json!({
        "step_index": step.step_index,
        "target_type": "activity",
        "target_id": step.step_id,
        "state": state,
        "started_at": step.started_at.map(|v| v.to_rfc3339()),
        "finished_at": step.finished_at.map(|v| v.to_rfc3339()),
        "duration_ms": duration_ms,
        "exit_code": null,
        "agent_response_json": null,
        "error_code": null,
        "error_message": step.error_message,
        "outcome": outcome,
    })
}

pub(super) async fn list_run_events(
    Ws(runtime): Ws,
    Path(id): Path<String>,
    Query(q): Query<RunEventsQuery>,
) -> Response {
    let run_id = match validate_id(&id) {
        Ok(id) => id,
        Err(message) => return bad_request(message),
    };
    let limit = bounded_limit(q.limit, RUN_EVENTS_DEFAULT_LIMIT);
    let offset = q.offset.unwrap_or(0);
    let kind = q
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let run_id = run_id.to_string();
    match blocking("run events", move || {
        let rows = runtime.list_v2_audit_events(V2AuditEventFilter {
            workspace_id: String::new(),
            run_id: Some(run_id),
            source: Some("v2_envelope".to_string()),
            limit: Some(RUN_EVENTS_MAX_SCAN_LINES + 1),
            ..Default::default()
        })?;
        let mut page: Vec<Value> = Vec::with_capacity(RUN_EVENTS_PAGE_CAPACITY_HINT);
        let mut matched: usize = 0;
        let mut lines_scanned: usize = 0;
        let mut budget_exceeded = false;

        for row in rows.into_iter().rev() {
            lines_scanned = lines_scanned.saturating_add(1);
            if lines_scanned > RUN_EVENTS_MAX_SCAN_LINES {
                budget_exceeded = true;
                break;
            }
            let value: Value = match serde_json::from_str(&row.payload_json) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if let Some(ref needle) = kind {
                let body_kind = value.get("body_kind").and_then(Value::as_str).unwrap_or("");
                if body_kind != needle {
                    continue;
                }
            }
            if matched < offset {
                matched = matched.saturating_add(1);
                continue;
            }
            page.push(value);
            matched = matched.saturating_add(1);
            if page.len() >= limit {
                break;
            }
        }

        Ok((page, budget_exceeded))
    })
    .await
    {
        Ok((page, budget_exceeded)) if budget_exceeded && page.len() < limit => (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({
                "error": "run-events audit rows exceed bounded scan budget; narrow the kind filter or reduce offset"
            })),
        )
            .into_response(),
        Ok((page, _)) => Json(Value::Array(page)).into_response(),
        Err(response) => *response,
    }
}

pub(super) async fn list_run_logs(
    Ws(runtime): Ws,
    Path(id): Path<String>,
    Query(q): Query<LimitQuery>,
) -> Response {
    let run_id = match validate_id(&id) {
        Ok(id) => id,
        Err(message) => return bad_request(message),
    };
    let limit = bounded_limit(q.limit, HISTORY_DEFAULT_LIMIT);
    let run_id = run_id.to_string();
    match blocking("run logs", move || {
        runtime.collect_run_cli_invocations(&run_id)
    })
    .await
    {
        Ok(records) => Json(Value::Array(
            records
                .into_iter()
                .take(limit)
                .map(run_cli_invocation_to_json)
                .collect(),
        ))
        .into_response(),
        Err(response) => *response,
    }
}

fn run_cli_invocation_to_json(record: RunCliInvocationRecord) -> Value {
    let stdout_preview = bounded_preview(&record.stdout);
    let stderr_preview = bounded_preview(&record.stderr);
    json!({
        "run_id": record.run_id,
        "event_id": record.event_id,
        "ts": record.ts.map(|ts| ts.to_rfc3339()),
        "step_id": record.step_id,
        "step_index": record.step_index,
        "provider": record.provider,
        "stdout_blob_ref": record.stdout_blob_ref,
        "stderr_blob_ref": record.stderr_blob_ref,
        "stdout_preview": stdout_preview.text,
        "stderr_preview": stderr_preview.text,
        "stdout_truncated": stdout_preview.truncated,
        "stderr_truncated": stderr_preview.truncated,
        "exit_code": record.exit_code,
        "timed_out": record.timed_out,
        "duration_ms": record.duration_ms,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Preview {
    text: String,
    truncated: bool,
}

fn bounded_preview(raw: &str) -> Preview {
    let mut out = String::new();
    let mut truncated = false;
    for (index, line) in raw.lines().enumerate() {
        if index >= RUN_LOG_PREVIEW_MAX_LINES {
            truncated = true;
            break;
        }
        let needed = line.len() + usize::from(!out.is_empty());
        if out.len().saturating_add(needed) > RUN_LOG_PREVIEW_MAX_BYTES {
            if out.is_empty() {
                for ch in line.chars() {
                    if out.len().saturating_add(ch.len_utf8()) > RUN_LOG_PREVIEW_MAX_BYTES {
                        break;
                    }
                    out.push(ch);
                }
            }
            truncated = true;
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
    }
    if raw.ends_with('\n') && !out.is_empty() && out.len() < RUN_LOG_PREVIEW_MAX_BYTES {
        out.push('\n');
    }
    Preview {
        text: redact_all(&out),
        truncated,
    }
}
