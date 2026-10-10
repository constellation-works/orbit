//! `GET /api/doctor`: the `orbit doctor` checks for the selected workspace,
//! read-only and cached (see [`crate::doctor_report`]).
//!
//! The rows carry the same fields as `orbit doctor --json`. A plain read
//! returns a recent cached report, running doctor only when there is none;
//! `?refresh=true` runs it again; `?cached=true` returns whatever is cached
//! and never runs it, which is what the dashboard's poll uses for the rail
//! count. Nothing here takes a repair or `--deep`: an unrecognised parameter
//! is refused rather than ignored, so a `fix_*` flag cannot look accepted.

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Json, Response};
use serde_json::{Value, json};

use super::bad_request;
use crate::doctor_report::DoctorRead;
use crate::state::{DashboardState, Ws};

/// `GET /api/doctor?workspace=<id>[&refresh=true|&cached=true]`.
pub(super) async fn doctor(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let read = match doctor_read(&params) {
        Ok(read) => read,
        Err(message) => return bad_request(message),
    };
    match state.doctor_reports().read(&runtime, read).await {
        Some((report, finished)) => Json(report.to_json(finished)).into_response(),
        None => Json(empty_report()).into_response(),
    }
}

fn doctor_read(params: &HashMap<String, String>) -> Result<DoctorRead, String> {
    let mut refresh = false;
    let mut cached = false;
    for (name, value) in params {
        let flag = match name.as_str() {
            // Consumed by the `Ws` extractor.
            "workspace" => continue,
            "refresh" => &mut refresh,
            "cached" => &mut cached,
            other => {
                return Err(format!(
                    "unsupported parameter '{other}': the dashboard runs only the read-only \
                     doctor checks; run repairs and `--deep` with `orbit doctor` on the host"
                ));
            }
        };
        *flag = match value.as_str() {
            "true" => true,
            "false" => false,
            _ => return Err(format!("'{name}' must be true or false")),
        };
    }
    match (refresh, cached) {
        (true, true) => Err("'refresh' and 'cached' cannot both be true".to_string()),
        (true, false) => Ok(DoctorRead::Refresh),
        (false, true) => Ok(DoctorRead::Peek),
        (false, false) => Ok(DoctorRead::Recent),
    }
}

/// `?cached=true` before any run: no report yet, and nothing was run.
fn empty_report() -> Value {
    json!({
        "ran_at": null,
        "age_ms": null,
        "duration_ms": null,
        "failures": null,
        "warnings": null,
        "checks": null,
    })
}
