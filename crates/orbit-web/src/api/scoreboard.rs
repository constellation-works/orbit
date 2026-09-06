//! Scoreboard endpoint: per-agent stats joined with metrics extras and denials.

use std::collections::BTreeMap;

use crate::state::Ws;
use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Datelike, Utc};
use orbit_cmd::DiagnosticsCommands;
use orbit_core::scoreboard_summary::ScoreboardWindow;
use orbit_core::{FailureIncidentQuery, OrbitRuntime};
use serde::Deserialize;
use serde_json::{Value, json};

use super::incidents::{ActorFailureRollup, ROLLUP_SCAN_LIMIT, agent_family_key, rollup_by_actor};
use super::server_error;

/// Query-string shape for `GET /api/scoreboard`.
///
/// `?window=<1h|24h|7d|30d|all>` scopes the summary. Missing param keeps
/// the legacy lifetime behavior. Unknown values produce HTTP 400.
#[derive(Debug, Default, Deserialize)]
pub(super) struct ScoreboardQuery {
    #[serde(default)]
    pub(super) window: Option<String>,
}

pub(super) async fn scoreboard(Ws(runtime): Ws, Query(query): Query<ScoreboardQuery>) -> Response {
    let window = match query.window.as_deref() {
        None => ScoreboardWindow::All,
        Some(raw) => match raw.parse::<ScoreboardWindow>() {
            Ok(w) => w,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": e.to_string() })),
                )
                    .into_response();
            }
        },
    };

    let summary = match runtime.generate_scoreboard_summary(Some(window)) {
        Ok(s) => s,
        Err(e) => return server_error(e),
    };
    let mut value = match serde_json::to_value(&summary) {
        Ok(v) => v,
        Err(e) => return server_error(orbit_core::OrbitError::Store(e.to_string())),
    };

    // Join MetricsEntry-derived per-actor stats and audit denials. A source
    // read/query failure is logged with full context and reported as an
    // explicit `unavailable` coverage note plus `null` per-agent fields —
    // never as a measured zero, which would be indistinguishable from a
    // source that genuinely observed no activity. See ORB-11201.
    //
    // One `now`/`since_window` pair scopes every join below (metrics extras,
    // denials, failure rollup) so the response mixes only compatible
    // populations for the requested window.
    let now = Utc::now();
    let since_window = window.duration().map(|d| now - d);

    let metrics_extras = match compute_metrics_extras(&runtime, since_window, now) {
        Ok(extras) => Some(extras),
        Err(e) => {
            tracing::error!(
                error = %e,
                source = "metrics_extras",
                window = window.as_str(),
                "scoreboard metrics-extras join failed; reporting retries/durations as unavailable"
            );
            None
        }
    };
    let denial_map: Option<BTreeMap<String, i64>> =
        match runtime.audit_denials_by_role(since_window.as_ref()) {
            Ok(rows) => Some(rows.into_iter().collect()),
            Err(e) => {
                tracing::error!(
                    error = %e,
                    source = "audit_denials_by_role",
                    window = window.as_str(),
                    "scoreboard denial-count join failed; reporting denials as unavailable"
                );
                None
            }
        };

    // ORB-10871: a repeated failure burst is one incident, not one failure per
    // raw row. The scoreboard keeps `failed_tool_calls` (the raw count) and
    // gains the grouped counts beside it, over the same window, so an operator
    // reads "how many things went wrong" and "how much evidence there is" as
    // two distinct numbers.
    let failure_rollup = match runtime.audit_failure_incidents(&FailureIncidentQuery {
        since: since_window,
        max_events: ROLLUP_SCAN_LIMIT,
        ..Default::default()
    }) {
        Ok(report) => Some(rollup_by_actor(&report)),
        Err(e) => {
            tracing::error!(
                error = %e,
                source = "audit_failure_incidents",
                window = window.as_str(),
                "scoreboard failure-incident join failed; reporting failure incidents as unavailable"
            );
            None
        }
    };

    if let Some(coverage) = value.get_mut("coverage").and_then(|v| v.as_object_mut()) {
        coverage.insert(
            "metrics_extras".to_string(),
            coverage_note(
                metrics_extras.is_some(),
                "Retry counts and step durations are measured for the requested window; an agent absent from the join has no recorded steps, not a read failure.",
                "Metrics log read failed for the requested window; avg_step_duration_ms, p95_wall_clock_ms, and retries are omitted (null) rather than shown as zero.",
            ),
        );
        coverage.insert(
            "denials".to_string(),
            coverage_note(
                denial_map.is_some(),
                "Denial counts are measured for the requested window; zero means no observed denials.",
                "Audit denial-count query failed for the requested window; denials is omitted (null) rather than shown as zero.",
            ),
        );
        coverage.insert(
            "failure_incidents".to_string(),
            coverage_note(
                failure_rollup.is_some(),
                "Failure incidents are measured for the requested window; zero means no observed failure incidents.",
                "Audit failure-incident query failed for the requested window; failure_incidents, unexpected_failure_incidents, and failure_incident_events are omitted (null) rather than shown as zero.",
            ),
        );
    }

    if let Some(agents) = value.get_mut("agents").and_then(|v| v.as_object_mut()) {
        apply_side_source_extras(
            agents,
            metrics_extras.as_ref(),
            denial_map.as_ref(),
            failure_rollup.as_ref(),
        );
    }

    Json(value).into_response()
}

/// Standard `{ "availability": ..., "detail": ... }` coverage note shape
/// (matching [`orbit_store`]'s snapshot `CoverageNote`), used here for
/// side-source joins that can fail independently of the primary summary.
fn coverage_note(available: bool, observed_detail: &str, unavailable_detail: &str) -> Value {
    if available {
        json!({ "availability": "observed", "detail": observed_detail })
    } else {
        json!({ "availability": "unavailable", "detail": unavailable_detail })
    }
}

/// Applies the three side-source joins onto each agent's JSON object.
///
/// `None` for a source means its upstream read/query failed (already logged
/// with context by the caller): every field that source would have populated
/// is set to `null`, never `0`, so a read failure can never be read as a
/// measured zero. `Some(map)` — even an empty one — means the source
/// succeeded, so an agent missing from it is a true, observed zero.
pub(super) fn apply_side_source_extras(
    agents: &mut serde_json::Map<String, Value>,
    metrics_extras: Option<&BTreeMap<String, MetricsExtras>>,
    denial_map: Option<&BTreeMap<String, i64>>,
    failure_rollup: Option<&BTreeMap<String, ActorFailureRollup>>,
) {
    // Collect all agent keys upfront so we can also surface metrics rows
    // that have no scoreboard counterpart yet.
    let existing_keys: Vec<String> = agents.keys().cloned().collect();
    for key in &existing_keys {
        let extras = metrics_extras.map(|m| m.get(key.as_str()).cloned().unwrap_or_default());
        let denials = denial_map.map(|m| lookup_denials_for_agent(m, key));
        let failures = failure_rollup.map(|m| lookup_failures_for_agent(m, key));
        if let Some(obj) = agents.get_mut(key.as_str()).and_then(|v| v.as_object_mut()) {
            insert_extras_fields(obj, extras.as_ref(), denials, failures.as_ref());
        }
    }
    // Surface metrics-only agents so retries/durations show even when no
    // task or token row exists for them yet. Only possible when the metrics
    // source itself succeeded — a failed source has no rows to surface.
    if let Some(metrics_extras) = metrics_extras {
        for (key, extras) in metrics_extras {
            if existing_keys.iter().any(|k| k == key) {
                continue;
            }
            let denials = denial_map.map(|m| lookup_denials_for_agent(m, key));
            let failures = failure_rollup.map(|m| lookup_failures_for_agent(m, key));
            let mut obj = serde_json::Map::new();
            obj.insert("tasks_completed".to_string(), json!(0));
            obj.insert("tokens".to_string(), json!({ "total": 0, "output": 0 }));
            obj.insert(
                "pr".to_string(),
                json!({ "review_comments": 0, "merged_clean": 0, "merged_with_revision": 0 }),
            );
            obj.insert("task_review".to_string(), json!({ "threads": 0 }));
            obj.insert("friction".to_string(), json!({ "reported": 0 }));
            obj.insert("tool_calls".to_string(), json!(0));
            obj.insert("failed_tool_calls".to_string(), json!(0));
            insert_extras_fields(&mut obj, Some(extras), denials, failures.as_ref());
            agents.insert(key.clone(), Value::Object(obj));
        }
    }
}

/// Writes the seven side-source-derived fields onto one agent's JSON object.
/// `None` for a joined value means its source failed and the field becomes
/// `null`; `Some` (including a zeroed default) means the source succeeded.
fn insert_extras_fields(
    obj: &mut serde_json::Map<String, Value>,
    extras: Option<&MetricsExtras>,
    denials: Option<i64>,
    failures: Option<&ActorFailureRollup>,
) {
    obj.insert(
        "avg_step_duration_ms".to_string(),
        extras.map_or(Value::Null, |e| json!(e.avg_duration_ms)),
    );
    obj.insert(
        "retries".to_string(),
        extras.map_or(Value::Null, |e| json!(e.retry_count)),
    );
    obj.insert(
        "p95_wall_clock_ms".to_string(),
        extras.map_or(Value::Null, |e| json!(e.p95_duration_ms)),
    );
    obj.insert(
        "denials".to_string(),
        denials.map_or(Value::Null, |d| json!(d)),
    );
    obj.insert(
        "failure_incidents".to_string(),
        failures.map_or(Value::Null, |f| json!(f.incidents)),
    );
    obj.insert(
        "unexpected_failure_incidents".to_string(),
        failures.map_or(Value::Null, |f| json!(f.unexpected_incidents)),
    );
    obj.insert(
        "failure_incident_events".to_string(),
        failures.map_or(Value::Null, |f| json!(f.events)),
    );
}

/// Per-agent extras derived from `MetricsEntry` JSONL.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct MetricsExtras {
    pub(super) avg_duration_ms: i64,
    pub(super) p95_duration_ms: i64,
    pub(super) retry_count: i64,
}

// `pub(super)`: exercised directly by the sibling `api/tests/scoreboard.rs`
// unit tests (window/month-boundary arithmetic), per the crate's sibling
// test-layout convention — see docs/design-patterns/test_layout.md.
pub(super) fn compute_metrics_extras(
    runtime: &OrbitRuntime,
    since: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<BTreeMap<String, MetricsExtras>, orbit_core::OrbitError> {
    use orbit_types::identity::ActorIdentity;

    // A finite window only needs its own partitions; lifetime (`since ==
    // None`) enumerates every partition that has ever been written so
    // records older than a couple of months are still counted.
    let months = match since {
        Some(since) => months_in_range(since, now),
        None => runtime.list_metrics_months()?,
    };

    let mut by_actor: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    let mut retries: BTreeMap<String, i64> = BTreeMap::new();
    for month in &months {
        let entries = match runtime.read_metrics_entries(month) {
            Ok(e) => e,
            Err(orbit_core::OrbitError::InvalidInput(_)) => continue,
            Err(e) => return Err(e),
        };
        for entry in entries {
            // A month partition covers a superset of the requested range
            // (e.g. the first/last day of the month), so re-check each
            // record against the exact boundary.
            if since.is_some_and(|since| entry.ts < since) || entry.ts > now {
                continue;
            }
            let key = match &entry.actor_identity {
                ActorIdentity::Agent { model } if !model.is_empty() => model.clone(),
                ActorIdentity::Human { label } if !label.is_empty() => label.clone(),
                _ => continue,
            };
            *retries.entry(key.clone()).or_insert(0) += entry.retry_count as i64;
            if let Some(d) = entry.step_duration_ms {
                by_actor.entry(key).or_default().push(d);
            }
        }
    }

    let mut out: BTreeMap<String, MetricsExtras> = BTreeMap::new();
    for (key, durations) in by_actor {
        let mut sorted = durations.clone();
        sorted.sort_unstable();
        let sum: u128 = sorted.iter().map(|d| *d as u128).sum();
        let avg = if sorted.is_empty() {
            0
        } else {
            (sum / sorted.len() as u128) as i64
        };
        let idx = ((sorted.len() as f64) * 0.95).ceil() as usize;
        let idx = idx.min(sorted.len()).saturating_sub(1);
        let p95 = sorted.get(idx).copied().unwrap_or(0) as i64;
        let retry = retries.remove(&key).unwrap_or(0);
        out.insert(
            key,
            MetricsExtras {
                avg_duration_ms: avg,
                p95_duration_ms: p95,
                retry_count: retry,
            },
        );
    }
    // Carry over retries-only actors that had no duration samples.
    for (key, count) in retries {
        out.entry(key).or_insert(MetricsExtras {
            avg_duration_ms: 0,
            p95_duration_ms: 0,
            retry_count: count,
        });
    }
    Ok(out)
}

/// Ascending `YYYY-MM` partitions spanning `since..=now`, inclusive of both
/// endpoints' months. Steps by calendar month rather than a fixed day offset
/// so a window whose start falls on an early-month date (e.g. subtracting 30
/// days from March 1st) still includes every month it actually touches.
pub(super) fn months_in_range(since: DateTime<Utc>, now: DateTime<Utc>) -> Vec<String> {
    let (mut year, mut month) = (since.year(), since.month());
    let (end_year, end_month) = (now.year(), now.month());

    let mut months = Vec::new();
    loop {
        months.push(format!("{year:04}-{month:02}"));
        if year > end_year || (year == end_year && month >= end_month) {
            break;
        }
        if month == 12 {
            month = 1;
            year += 1;
        } else {
            month += 1;
        }
    }
    months
}

/// Looks up an agent's grouped failure counts. The rollup is already keyed by
/// canonical agent family, so a scoreboard key that is itself a model label
/// (metrics-only rows can be) is normalized the same way before matching.
fn lookup_failures_for_agent(
    rollup: &BTreeMap<String, ActorFailureRollup>,
    agent_key: &str,
) -> ActorFailureRollup {
    if let Some(found) = rollup.get(agent_key) {
        return *found;
    }
    rollup
        .get(&agent_family_key(agent_key))
        .copied()
        .unwrap_or_default()
}

/// Looks up the SQLite per-role denials for a scoreboard agent key. The audit
/// schema stores `role` as a free-form string (often the bare agent name), so
/// we accept either a direct match or a model-prefix match.
fn lookup_denials_for_agent(map: &BTreeMap<String, i64>, agent_key: &str) -> i64 {
    if let Some(v) = map.get(agent_key) {
        return *v;
    }
    // Match by leading agent-name portion of `agent / model` keys.
    if let Some(idx) = agent_key.find(" / ") {
        let head = &agent_key[..idx];
        if let Some(v) = map.get(head) {
            return *v;
        }
    }
    0
}
