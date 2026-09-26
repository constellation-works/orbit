use axum::body::to_bytes;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Duration, TimeZone, Timelike, Utc};
use serde::Deserialize;
use serde_json::json;

pub(super) const HISTORY_DEFAULT_LIMIT: usize = 50;
pub(super) const HISTORY_MAX_LIMIT: usize = 200;
/// Default time window for header tile counts when `?since=` is omitted.
pub(super) const DEFAULT_SUMMARY_WINDOW: &str = "24h";

#[derive(Deserialize, Default)]
pub(super) struct LimitQuery {
    #[serde(default)]
    pub(super) limit: Option<usize>,
}

#[derive(Deserialize)]
pub(super) struct DiagnosticsQuery {
    #[serde(default)]
    pub(super) month: Option<String>,
    #[serde(default)]
    pub(super) limit: Option<usize>,
}

#[derive(Deserialize, Default)]
pub(super) struct AuditQuery {
    #[serde(default)]
    pub(super) since: Option<String>,
    #[serde(default)]
    pub(super) tool: Option<String>,
    #[serde(default)]
    pub(super) status: Option<String>,
    #[serde(default)]
    pub(super) role: Option<String>,
    #[serde(default)]
    pub(super) workspace_id: Option<String>,
    #[serde(default)]
    pub(super) caller_machine: Option<String>,
    #[serde(default)]
    pub(super) process_machine: Option<String>,
    #[serde(default)]
    pub(super) transport: Option<String>,
    #[serde(default)]
    pub(super) capability: Option<String>,
    #[serde(default)]
    pub(super) origin_session: Option<String>,
    #[serde(default)]
    pub(super) mcp_call: Option<String>,
    #[serde(default)]
    pub(super) job_run_id: Option<String>,
    #[serde(default)]
    pub(super) lease: Option<String>,
    /// Filters audit events by orbit invocation id. The SQLite `audit_events`
    /// schema has no `run_id` column; `run_id` here is a backward-compat alias
    /// of `execution_id` (T20260427-26). When both are supplied, `execution_id`
    /// takes precedence.
    #[serde(default)]
    pub(super) execution_id: Option<String>,
    #[serde(default)]
    pub(super) run_id: Option<String>,
    #[serde(default)]
    pub(super) q: Option<String>,
    /// fsProfile filter. The SQLite `audit_events` schema has no first-class
    /// `profile` column; matching is best-effort against `arguments_json`. The
    /// canonical denials view (`/api/diagnostics/denials`) reads the v2 envelope
    /// JSONL where `profile` is a typed field.
    #[serde(default)]
    pub(super) profile: Option<String>,
    #[serde(default)]
    pub(super) limit: Option<usize>,
    #[serde(default)]
    pub(super) offset: Option<usize>,
}

#[derive(Deserialize, Default)]
pub(super) struct AuditSummaryQuery {
    #[serde(default)]
    pub(super) since: Option<String>,
    #[serde(default)]
    pub(super) denial_threshold: Option<i64>,
}

#[derive(Deserialize, Default)]
pub(super) struct DenialsQuery {
    #[serde(default)]
    pub(super) since: Option<String>,
    /// `fs`, `tool`, or omitted (combined).
    #[serde(default)]
    pub(super) kind: Option<String>,
    #[serde(default)]
    pub(super) profile: Option<String>,
    #[serde(default)]
    pub(super) agent: Option<String>,
}

#[derive(Deserialize, Default)]
pub(super) struct RunEventsQuery {
    #[serde(default)]
    pub(super) kind: Option<String>,
    #[serde(default)]
    pub(super) limit: Option<usize>,
    #[serde(default)]
    pub(super) offset: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub(super) struct LogQuery {
    #[serde(default)]
    pub(super) limit: Option<usize>,
    #[serde(default)]
    pub(super) target: Option<String>,
    #[serde(default)]
    pub(super) level: Option<String>,
    #[serde(default)]
    pub(super) since: Option<String>,
    /// Byte offset to resume `/log/stream` from. Ignored by `/log` snapshot.
    /// Overridden by `Last-Event-ID` when both are present.
    #[serde(default)]
    pub(super) from: Option<u64>,
}

pub(super) fn current_year_month_utc() -> String {
    Utc::now().format("%Y-%m").to_string()
}

/// Validates a `YYYY-MM` string with month range 01..=12.
pub(super) fn validate_year_month(raw: &str) -> Result<(), orbit_core::OrbitError> {
    let bytes = raw.as_bytes();
    let format_ok = bytes.len() == 7
        && bytes[4] == b'-'
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[5..].iter().all(u8::is_ascii_digit);
    if !format_ok {
        return Err(orbit_core::OrbitError::InvalidInput(format!(
            "month must be in YYYY-MM format, got '{raw}'"
        )));
    }
    let month: u32 = raw[5..].parse().unwrap_or(0);
    if !(1..=12).contains(&month) {
        return Err(orbit_core::OrbitError::InvalidInput(format!(
            "month component must be 01-12, got '{raw}'"
        )));
    }
    Ok(())
}

pub(super) fn month_bounds_utc(
    raw: &str,
) -> Result<(DateTime<Utc>, DateTime<Utc>), orbit_core::OrbitError> {
    validate_year_month(raw)?;
    let year = raw[..4].parse::<i32>().map_err(|_| {
        orbit_core::OrbitError::InvalidInput(format!("invalid year component in '{raw}'"))
    })?;
    let month = raw[5..].parse::<u32>().map_err(|_| {
        orbit_core::OrbitError::InvalidInput(format!("invalid month component in '{raw}'"))
    })?;
    let start = Utc
        .with_ymd_and_hms(year, month, 1, 0, 0, 0)
        .single()
        .ok_or_else(|| {
            orbit_core::OrbitError::InvalidInput(format!("invalid month boundary for '{raw}'"))
        })?;
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let next_start = Utc
        .with_ymd_and_hms(next_year, next_month, 1, 0, 0, 0)
        .single()
        .ok_or_else(|| {
            orbit_core::OrbitError::InvalidInput(format!("invalid month boundary for '{raw}'"))
        })?;
    Ok((start, next_start - Duration::nanoseconds(1)))
}

pub(super) fn truncate_to_hour(ts: DateTime<Utc>) -> DateTime<Utc> {
    ts.with_minute(0)
        .and_then(|t| t.with_second(0))
        .and_then(|t| t.with_nanosecond(0))
        .unwrap_or(ts)
}

pub(super) fn bounded_limit(requested: Option<usize>, default: usize) -> usize {
    requested.unwrap_or(default).min(HISTORY_MAX_LIMIT)
}

pub(super) fn validate_id(id: &str) -> Result<&str, String> {
    let valid = !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if valid {
        Ok(id)
    } else {
        Err("id must contain only ASCII letters, digits, '-' or '_'".to_string())
    }
}

pub(super) fn non_empty_string(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

pub(super) fn map_runtime_error(e: orbit_core::OrbitError) -> Response {
    match e {
        orbit_core::OrbitError::InvalidInput(msg) => bad_request(msg),
        orbit_core::OrbitError::InvalidInputDiagnostic { message, .. } => bad_request(message),
        orbit_core::OrbitError::NotFound {
            kind: orbit_core::NotFoundKind::Task,
            id,
        } => not_found(format!("task not found: {id}")),
        orbit_core::OrbitError::NotFound {
            kind: orbit_core::NotFoundKind::Friction,
            id,
        } => not_found(format!("friction record not found: {id}")),
        orbit_core::OrbitError::NotFound {
            kind: orbit_core::NotFoundKind::Job,
            id,
        } => not_found(format!("job not found: {id}")),
        orbit_core::OrbitError::NotFound {
            kind: orbit_core::NotFoundKind::JobRun,
            id,
        } => not_found(format!("run not found: {id}")),
        error @ orbit_core::OrbitError::RemoteArtifactUnavailable { .. } => {
            artifact_conflict(error, "remote_artifact_unavailable")
        }
        error @ orbit_core::OrbitError::ArtifactNotLocal { .. } => {
            artifact_conflict(error, "artifact_not_local")
        }
        error @ orbit_core::OrbitError::FrictionNotLocal(_) => friction_not_local_conflict(error),
        error @ orbit_core::OrbitError::ShipRunInFlight { .. } => {
            ship_run_in_flight_conflict(error)
        }
        // [ORB-10709] Another operator holds this workspace's claim. A 409 for
        // the same reason a duplicate dispatch is one: the request is
        // well-formed, and the caller can retry once the claim lapses or is
        // released.
        error @ orbit_core::OrbitError::WorkspaceClaimHeld(_) => {
            workspace_claim_held_conflict(error)
        }
        // [ORB-10965] A duplicate run start that lost to the incumbent owner is
        // a conflict, not a server fault: the request was well-formed and
        // another worker simply got there first.
        orbit_core::OrbitError::JobRunStartConflict(message) => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": message,
                "code": "job_run_start_conflict",
            })),
        )
            .into_response(),
        other => server_error(other),
    }
}

/// [ORB-10544] The stable 409 the ship endpoint has always returned for a
/// duplicate dispatch, now projected from the shared submission path's typed
/// conflict rather than from an endpoint-local policy.
fn ship_run_in_flight_conflict(error: orbit_core::OrbitError) -> Response {
    let (task_id, run_id) = error
        .ship_run_in_flight()
        .map_or_else(Default::default, |(task_id, run_id)| {
            (task_id.to_string(), run_id.to_string())
        });
    (
        StatusCode::CONFLICT,
        Json(json!({
            "error": error.to_string(),
            "code": "ship_run_in_flight",
            "run_id": run_id,
            "task_id": task_id,
        })),
    )
        .into_response()
}

/// [ORB-10709] Another operator holds this workspace's claim. A 409 for the
/// same reason a duplicate dispatch is one: the request is well-formed, and the
/// caller can retry once the claim lapses or is released. The body names the
/// holder and the expiry — never the holder's token.
fn workspace_claim_held_conflict(error: orbit_core::OrbitError) -> Response {
    let claim = error.workspace_claim_held();
    let body = json!({
        "error": error.to_string(),
        "code": "workspace_claim_held",
        "operation": claim.map(|claim| claim.operation.as_str()),
        "holder": claim.map(|claim| claim.holder.as_str()),
        "claim_id": claim.map(|claim| claim.claim_id.as_str()),
        "expires_at": claim.map(|claim| claim.expires_at.as_str()),
    });
    (StatusCode::CONFLICT, Json(body)).into_response()
}

fn friction_not_local_conflict(error: orbit_core::OrbitError) -> Response {
    let details = error.friction_not_local_details();
    (
        StatusCode::CONFLICT,
        Json(json!({
            "error": error.to_string(),
            "code": "friction_not_local",
            "friction_id": details.map(|details| details.friction_id.as_str()),
            "task_id": details.map(|details| details.task_id.as_str()),
            "workspace_id": details.map(|details| details.workspace_id.as_str()),
            "found_in": details.map(|details| details.found_in.as_slice()),
        })),
    )
        .into_response()
}

fn artifact_conflict(error: orbit_core::OrbitError, code: &'static str) -> Response {
    let artifact_origin = error.artifact_origin().cloned();
    (
        StatusCode::CONFLICT,
        Json(json!({
            "error": error.to_string(),
            "code": code,
            "artifact_origin": artifact_origin,
        })),
    )
        .into_response()
}

pub(super) fn bad_request(message: String) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message }))).into_response()
}

pub(super) fn not_found(message: String) -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": message }))).into_response()
}

pub(super) fn server_error(e: orbit_core::OrbitError) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": e.to_string() })),
    )
        .into_response()
}

/// Normalize framework-generated client errors to the dashboard's JSON error
/// contract. Extractor rejections occur before handlers run, so this boundary
/// covers every route without repeating rejection handling in each handler.
pub(super) async fn json_client_error(response: Response) -> Response {
    if !response.status().is_client_error()
        || response
            .headers()
            .get(header::CONTENT_TYPE)
            .is_some_and(|value| value.as_bytes().starts_with(b"application/json"))
    {
        return response;
    }

    let (parts, body) = response.into_parts();
    let message = match to_bytes(body, 64 * 1024).await {
        Ok(body) => String::from_utf8_lossy(&body).into_owned(),
        Err(error) => format!("failed to read error response: {error}"),
    };
    let mut json_response = (parts.status, Json(json!({ "error": message }))).into_response();

    for (name, value) in &parts.headers {
        if name != header::CONTENT_TYPE {
            json_response.headers_mut().insert(name, value.clone());
        }
    }

    json_response
}

/// Run a blocking runtime call on the blocking pool instead of the async
/// worker that is serving the request.
///
/// Every `OrbitRuntime` task mutation descends into an exclusive `flock` with
/// no timeout. Calling one straight from a handler parks a tokio worker for
/// the whole wait, so a burst of concurrent writes parks every worker and the
/// server stops answering — the `ReadTimeout`-then-`ConnectError` shape of
/// F2026-07-119. `label` names the operation in the panic-propagation error.
pub(super) async fn blocking<T, F>(label: &str, op: F) -> Result<T, Box<Response>>
where
    F: FnOnce() -> Result<T, orbit_core::OrbitError> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(op).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(Box::new(map_runtime_error(e))),
        Err(join_err) => Err(Box::new(server_error(orbit_core::OrbitError::Execution(
            format!("{label} panicked: {join_err}"),
        )))),
    }
}
