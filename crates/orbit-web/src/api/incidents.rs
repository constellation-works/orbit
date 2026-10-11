//! `GET /api/audit/incidents` — grouped failure incidents [ORB-10871].
//!
//! The dashboard used to read raw failed audit rows as a failure count, so one
//! repeated refusal burst rendered as hundreds of independent quality
//! failures. This endpoint reports the incident view *alongside* its raw
//! denominators — never instead of them — so a scorecard can say "N incidents
//! collapsed from M failed events of T events in the last 24h" without either
//! number being inferred.
//!
//! Every incident carries the ids of the raw rows it grouped, so the raw Audit
//! view and its exports stay the authority on evidence; nothing here filters,
//! rewrites, or hides a row.

use std::collections::BTreeMap;

use axum::extract::Query;
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Utc};
use orbit_common::security::redaction::redact_all;
use orbit_core::{
    FailureClass, FailureIncident, FailureIncidentQuery, FailureIncidentReport, IncidentEventRef,
    JOB_RUN_LIFECYCLE_LABEL, LIFECYCLE_DIAGNOSTIC_LABEL, OrbitError, PropagationLink,
};
use orbit_types::identity::{infer_agent_family_from_model, normalize_attribution_label};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    DEFAULT_SUMMARY_WINDOW, HISTORY_MAX_LIMIT, bad_request, bounded_limit, map_runtime_error,
    server_error,
};
use crate::parse::parse_since;
use crate::state::Ws;

/// Default number of incidents returned. Incidents are already a collapsed
/// view, so a page this size covers far more raw evidence than the same number
/// of audit rows would.
const INCIDENTS_DEFAULT_LIMIT: usize = 50;

/// `?since=<24h|7d|RFC3339>&class=<deferred|denied|diagnostic|expected|unexpected>&limit=`.
#[derive(Debug, Default, Deserialize)]
pub(super) struct IncidentsQuery {
    #[serde(default)]
    pub(super) since: Option<String>,
    #[serde(default)]
    pub(super) class: Option<String>,
    #[serde(default)]
    pub(super) role: Option<String>,
    #[serde(default)]
    pub(super) limit: Option<usize>,
}

pub(super) async fn list_failure_incidents(
    Ws(runtime): Ws,
    Query(query): Query<IncidentsQuery>,
) -> Response {
    let raw_since = query
        .since
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_SUMMARY_WINDOW)
        .to_string();
    // `all` is the shared dashboard window's lifetime setting; it is a valid
    // scope here (no cutoff), not a malformed duration.
    let since = if raw_since == "all" {
        None
    } else {
        match parse_since(&raw_since) {
            Ok(since) => Some(since),
            Err(e) => return map_runtime_error(e),
        }
    };
    let class_filter = match query
        .class
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => match parse_class(value) {
            Some(class) => Some(class),
            None => {
                return bad_request(format!(
                    "unknown failure class: {value} (expected deferred, denied, diagnostic, expected, or unexpected)"
                ));
            }
        },
        None => None,
    };
    let limit = bounded_limit(query.limit, INCIDENTS_DEFAULT_LIMIT);
    let role = query.role.filter(|role| !role.is_empty());

    let runtime_clone = runtime.clone();
    let incident_query = FailureIncidentQuery {
        since,
        role: role.clone(),
        ..Default::default()
    };
    // Both the grouping scan and the total-event count are synchronous SQLite
    // reads; keep them off the async executor.
    let bundle = match tokio::task::spawn_blocking(move || {
        let report = runtime_clone.audit_failure_incidents(&incident_query)?;
        let total_events = runtime_clone.audit_event_stats(since, None)?.total;
        Ok::<_, OrbitError>((report, total_events))
    })
    .await
    {
        Ok(Ok(bundle)) => bundle,
        Ok(Err(e)) => return server_error(e),
        Err(join_err) => {
            return server_error(OrbitError::Execution(format!(
                "failure incident aggregation panicked: {join_err}"
            )));
        }
    };
    let (report, total_events) = bundle;

    Json(incidents_payload(
        &report,
        &raw_since,
        since,
        total_events,
        class_filter,
        limit,
        role.as_deref(),
    ))
    .into_response()
}

fn parse_class(raw: &str) -> Option<FailureClass> {
    FailureClass::ALL
        .into_iter()
        .find(|class| class.as_str() == raw)
}

/// Projects a report into the dashboard payload.
///
/// Widened to `pub(super)` so `api/tests/incidents.rs` can pin the projection
/// (denominators, class labels, evidence refs) without a live SQLite runtime.
pub(super) fn incidents_payload(
    report: &FailureIncidentReport,
    window: &str,
    since: Option<DateTime<Utc>>,
    total_events: i64,
    class_filter: Option<FailureClass>,
    limit: usize,
    role: Option<&str>,
) -> Value {
    let selected: Vec<&FailureIncident> = report
        .incidents
        .iter()
        .filter(|incident| class_filter.is_none_or(|class| incident.class == class))
        .collect();
    let shown: Vec<Value> = selected
        .iter()
        .take(limit)
        .map(|incident| incident_to_json(incident))
        .collect();
    let selected_class = class_filter.map(FailureClass::as_str);
    let matching_raw_event_count = selected_class.map_or(report.raw_failed_events, |class| {
        report.raw_events_by_class.get(class).copied().unwrap_or(0)
    });
    let matching_affected_run_count = selected_class.map_or(report.affected_run_count, |class| {
        report
            .affected_runs_by_class
            .get(class)
            .copied()
            .unwrap_or(0)
    });

    json!({
        "window": window,
        "since": since.map(|since| since.to_rfc3339()),
        "role": role,
        "class": class_filter.map(FailureClass::as_str),
        // Denominators. Every count the UI renders states what it is out of:
        // incidents collapse failed events, failed events sit inside all
        // events, and all of it is scoped to `window`.
        "total_events": total_events,
        "raw_failed_events": report.raw_failed_events,
        "incident_count": report.incident_count(),
        "affected_run_count": report.affected_run_count,
        "job_run_lifecycle_events": report.job_run_lifecycle_events,
        "job_run_lifecycle_incidents": report.job_run_lifecycle_incidents,
        "job_run_lifecycle_label": JOB_RUN_LIFECYCLE_LABEL,
        "lifecycle_diagnostic_events": report.lifecycle_diagnostic_events,
        "lifecycle_diagnostic_incidents": report.lifecycle_diagnostic_incidents,
        "lifecycle_diagnostic_affected_run_count": report.lifecycle_diagnostic_affected_run_count,
        "lifecycle_diagnostic_label": LIFECYCLE_DIAGNOSTIC_LABEL,
        "matching_incident_count": selected.len(),
        "matching_raw_event_count": matching_raw_event_count,
        "matching_affected_run_count": matching_affected_run_count,
        "shown_incident_count": shown.len(),
        "limit": limit,
        "raw_events_by_class": report.raw_events_by_class,
        "incidents_by_class": report.incidents_by_class,
        "affected_runs_by_class": report.affected_runs_by_class,
        "failure_categories": failure_category_summaries(report),
        "class_labels": FailureClass::ALL
            .into_iter()
            .map(|class| (class.as_str().to_string(), json!(class.label())))
            .collect::<serde_json::Map<_, _>>(),
        "truncated": report.truncated,
        "incidents": shown,
    })
}

/// One canonical projection for the mutually exclusive failure classes.
/// Both summary endpoints use it so their labels and denominators cannot
/// drift apart.
pub(super) fn failure_category_summaries(report: &FailureIncidentReport) -> Value {
    let mut categories = serde_json::Map::new();
    for class in FailureClass::ALL {
        let key = class.as_str();
        categories.insert(
            key.to_string(),
            json!({
                "label": class.label(),
                "incidents": report.incidents_by_class.get(key).copied().unwrap_or(0),
                "raw_events": report.raw_events_by_class.get(key).copied().unwrap_or(0),
                "affected_runs": report.affected_runs_by_class.get(key).copied().unwrap_or(0),
            }),
        );
    }
    Value::Object(categories)
}

fn incident_to_json(incident: &FailureIncident) -> Value {
    json!({
        "incident_id": incident.incident_id,
        "signature": redact_all(&incident.signature),
        "class": incident.class.as_str(),
        "class_label": incident.class.label(),
        "actor": incident.role,
        "surface": incident.surface,
        "activity_id": incident.activity_id,
        "message": incident.message.as_deref().map(redact_all),
        "event_count": incident.event_count,
        "root_event_count": incident.root_event_count,
        "propagated_event_count": incident.propagated_event_count(),
        "first_ts": incident.first_ts.to_rfc3339(),
        "last_ts": incident.last_ts.to_rfc3339(),
        "run_ids": incident.run_ids,
        "task_ids": incident.task_ids,
        "has_tool_identity": incident.has_tool_identity,
        "sample_events": incident
            .sample_events
            .iter()
            .map(event_ref_to_json)
            .collect::<Vec<_>>(),
        "events": incident
            .events
            .iter()
            .map(event_ref_to_json)
            .collect::<Vec<_>>(),
        "propagation": incident
            .propagation
            .iter()
            .map(propagation_to_json)
            .collect::<Vec<_>>(),
        // [ORB-15434] Incidents sharing `cause.key` are one problem seen at
        // several surfaces; `basis` names the evidence that grouped them.
        "cause": {
            "key": incident.cause.key,
            "label": redact_all(&incident.cause.label),
            "basis": incident.cause.basis.as_str(),
        },
    })
}

fn event_ref_to_json(event: &IncidentEventRef) -> Value {
    json!({
        "id": event.id,
        "ts": event.ts.to_rfc3339(),
        "execution_id": event.execution_id,
        "status": event.status,
        "actor": event.role,
        "surface": event.surface,
        "run_id": event.run_id,
        "task_id": event.task_id,
        "tool": event.tool_name,
        "activity_id": event.activity_id,
        "message": event.message.as_deref().map(redact_all),
    })
}

fn propagation_to_json(link: &PropagationLink) -> Value {
    json!({
        "signature": redact_all(&link.signature),
        "surface": link.surface,
        "activity_id": link.activity_id,
        "event_count": link.event_count,
        "first_ts": link.first_ts.to_rfc3339(),
        "last_ts": link.last_ts.to_rfc3339(),
        "message": link.message.as_deref().map(redact_all),
        "sample_events": link
            .sample_events
            .iter()
            .map(event_ref_to_json)
            .collect::<Vec<_>>(),
    })
}

/// Incident counts per agent family, plus the raw failed-event total they
/// collapsed. Both are needed: the scoreboard renders them as a labeled pair
/// rather than a bare failure number.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct ActorFailureRollup {
    pub(super) incidents: i64,
    pub(super) events: i64,
    pub(super) unexpected_incidents: i64,
}

/// Rolls a report up by agent family, using the same label normalization the
/// scoreboard's own audit overlays use, so the incident counts land on the
/// same row as `tool_calls` / `failed_tool_calls` instead of on a parallel
/// model-named key. Kept here (rather than in `scoreboard.rs`) so the incident
/// contract has one owner.
pub(super) fn rollup_by_actor(
    report: &FailureIncidentReport,
) -> BTreeMap<String, ActorFailureRollup> {
    let mut out: BTreeMap<String, ActorFailureRollup> = BTreeMap::new();
    for incident in &report.incidents {
        let family = agent_family_key(&incident.role);
        if family.is_empty() {
            continue;
        }
        let entry = out.entry(family).or_default();
        entry.incidents += 1;
        entry.events += incident.event_count as i64;
        if incident.class == FailureClass::Unexpected {
            entry.unexpected_incidents += 1;
        }
    }
    out
}

/// Maps a free-form audit `role` onto a canonical agent family.
pub(super) fn agent_family_key(role: &str) -> String {
    let normalized = normalize_attribution_label(role, None);
    infer_agent_family_from_model(&normalized).unwrap_or(normalized)
}

/// Scan cap for the summary/scoreboard rollups, which only need counts. Held
/// well below the store default so a header tile can never become the slowest
/// query on the page.
pub(super) const ROLLUP_SCAN_LIMIT: usize = HISTORY_MAX_LIMIT * 50;
