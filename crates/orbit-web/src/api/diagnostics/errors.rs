//! Process-log and agent-stderr error rows with coverage.

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Utc};
use orbit_common::security::redaction::redact_all;
use orbit_common::storage::blob_store::BlobStore;
use orbit_core::runtime::audit::run::RunAuditStep;
use orbit_core::{JobRunState, OrbitRuntime, V2AuditEventFilter};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::super::{DiagnosticsQuery, HISTORY_DEFAULT_LIMIT, bounded_limit, map_runtime_error};
use super::audit::audit_blob_store;
use super::friction::{EnvelopeLinks, enclosing_step_id_for_event};
use super::metrics::DiagnosticsRange;
use crate::log_format::{
    Filters as LogFilters, format_message_html, read_recent_matching_events_across_segments,
    resolve_log_path,
};
use crate::runtime_memo::DIAGNOSTICS_ERRORS_TTL;
use crate::state::{DashboardState, Ws};

fn read_blob_text_best_effort(blob_store: &BlobStore, blob_ref: &str) -> String {
    blob_store
        .read(blob_ref)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

pub(in crate::api) async fn list_diagnostics_errors(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Query(q): Query<DiagnosticsQuery>,
) -> Response {
    let range = match DiagnosticsRange::from_query(&q, false) {
        Ok(range) => range,
        Err(error) => return map_runtime_error(error),
    };
    let limit = bounded_limit(q.limit, HISTORY_DEFAULT_LIMIT);
    let key = (range.key.clone(), limit);
    // Reads up to 50k audit rows and a blob per agent invocation, and every
    // Errors tab polls it: memoized so overlapping polls share one scan, and
    // computed on the blocking pool, not the worker serving the request.
    let compute_runtime = Arc::clone(&runtime);
    match state
        .diagnostics_errors_memo()
        .get_or_compute(&runtime, key, DIAGNOSTICS_ERRORS_TTL, move || {
            diagnostics_errors(&compute_runtime, &range, limit)
        })
        .await
    {
        Ok(rows) => Json((*rows).clone()).into_response(),
        Err(error) => map_runtime_error(error),
    }
}

/// The Errors feed: `items` newest first, the window start `since` (null for
/// `all`), and `coverage_since`, the instant from which both sources were read
/// completely. It equals `since` unless log retention, the process-log scan
/// cap or the stderr blob cap stopped a source later than the window start.
fn diagnostics_errors(
    runtime: &OrbitRuntime,
    range: &DiagnosticsRange,
    limit: usize,
) -> Result<Value, orbit_core::OrbitError> {
    let mut coverage_since = range.since;
    let rows = if limit == 0 {
        Vec::new()
    } else {
        let (rows, process_coverage, stderr_coverage) =
            diagnostics_error_rows(runtime, range, limit)?;
        coverage_since = [coverage_since, process_coverage, stderr_coverage]
            .into_iter()
            .flatten()
            .max();
        rows
    };
    Ok(json!({
        "items": rows,
        "since": range.since.map(|since| since.to_rfc3339()),
        "coverage_since": coverage_since.map(|since| since.to_rfc3339()),
    }))
}

type ErrorRowsWithCoverage = (Vec<Value>, Option<DateTime<Utc>>, Option<DateTime<Utc>>);

fn diagnostics_error_rows(
    runtime: &OrbitRuntime,
    range: &DiagnosticsRange,
    limit: usize,
) -> Result<ErrorRowsWithCoverage, orbit_core::OrbitError> {
    // Read beyond the display cap so duplicate events cannot crowd out distinct failures.
    let (mut rows, process_coverage) = global_error_rows_in_range(range, 50_000)?;
    rows.sort_by_key(|row| std::cmp::Reverse(row_timestamp(row)));
    let mut seen_process = HashSet::new();
    rows.retain(|row| {
        row["event_id"]
            .as_str()
            .is_none_or(|id| seen_process.insert(id.to_string()))
    });
    // The process log is host-global, so a run-affiliated row may belong to
    // another workspace. Only the selected workspace can resolve its runs'
    // steps, and a foreign row would keep the generic `step <id> finished
    // error` text; rows for runs this workspace does not own are dropped.
    // Filtering precedes the cap so foreign rows cannot crowd out local ones.
    let workspace_id = runtime.workspace_id()?;
    let mut runs = HashMap::new();
    let mut scoped = Vec::new();
    for mut row in rows {
        if scoped.len() >= limit {
            break;
        }
        let Some(run) = row["job_run"].as_str().map(str::to_string) else {
            scoped.push(row);
            continue;
        };
        let (owned, steps, succeeded_at) = runs.entry(run.clone()).or_insert_with(|| {
            let steps = runtime
                .collect_run_audit_step_attempts(&run)
                .unwrap_or_else(|error| {
                    tracing::warn!(%run, %error, "diagnostics step join unavailable");
                    Vec::new()
                });
            let record = runtime.sqlite_store().ok().and_then(|store| {
                store
                    .get_job_run_for_workspace(&workspace_id, &run)
                    .ok()
                    .flatten()
            });
            let owned = !steps.is_empty() || record.is_some();
            let succeeded_at = record
                .filter(|record| record.state == JobRunState::Success)
                .and_then(|record| record.finished_at);
            (owned, steps, succeeded_at)
        });
        if !*owned {
            continue;
        }
        if row["target"] == "orbit.job.step_finished" {
            let timestamp = row_timestamp(&row);
            row["recovered"] = json!(
                succeeded_at.is_some_and(|finished| timestamp.is_some_and(|ts| finished > ts))
            );
            if let Some(step) = step_attempt_for_error(steps, &row) {
                row["step_index"] = json!(step.step_index);
                if let Some(message) = step
                    .error_message
                    .as_deref()
                    .map(str::trim)
                    .filter(|message| !message.is_empty())
                {
                    row["message"] = json!(redact_all(message));
                }
            }
        }
        scoped.push(row);
    }
    let mut rows = scoped;
    let (stderr_rows, stderr_coverage) = agent_stderr_error_rows(runtime, range, limit)?;
    rows.extend(stderr_rows);
    rows.sort_by_key(|row| std::cmp::Reverse(row_timestamp(row)));
    let mut seen = HashSet::new();
    rows.retain(|row| {
        row["event_id"]
            .as_str()
            .is_none_or(|id| seen.insert(id.to_string()))
    });
    rows.truncate(limit);
    Ok((rows, process_coverage, stderr_coverage))
}

fn step_attempt_for_error<'a>(steps: &'a [RunAuditStep], row: &Value) -> Option<&'a RunAuditStep> {
    let timestamp = row_timestamp(row)?;
    let matches_step = |step: &&RunAuditStep| Some(step.step_id.as_str()) == row["step"].as_str();
    // Job tracing precedes the audit write. A row within an attempt's interval
    // joins its upcoming completion, even when that timestamp is slightly later.
    // Older/imported logs written after the audit join the preceding completion.
    let step = steps
        .iter()
        .filter(matches_step)
        .filter(|step| {
            step.started_at.is_some_and(|start| start <= timestamp)
                && step.finished_at.is_some_and(|finish| finish >= timestamp)
        })
        .min_by_key(|step| step.finished_at)
        .or_else(|| {
            steps
                .iter()
                .filter(matches_step)
                .filter(|step| step.finished_at.is_some_and(|finish| finish <= timestamp))
                .max_by_key(|step| step.finished_at)
        })?;
    (!matches!(step.outcome.as_deref(), Some("success" | "held"))).then_some(step)
}

/// Process ERROR rows from the active log and every rotated segment that
/// reaches into the window, with the instant the retained segments (or the
/// `limit` scan cap) actually start from when that is later than the window.
fn global_error_rows_in_range(
    range: &DiagnosticsRange,
    limit: usize,
) -> Result<(Vec<Value>, Option<DateTime<Utc>>), orbit_core::OrbitError> {
    let path = resolve_log_path(None)?;
    let filters = LogFilters::new(
        None,
        Some(crate::log_format::LevelFilter::Error),
        range.since,
    );
    let scanned =
        read_recent_matching_events_across_segments(&path, &filters, limit).map_err(|error| {
            orbit_core::OrbitError::Io(format!("read log {}: {error}", path.display()))
        })?;
    let rows = scanned
        .events
        .into_iter()
        .filter(|event| {
            event["timestamp"]
                .as_str()
                .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
                .is_some_and(|ts| range.contains(ts.with_timezone(&Utc)))
        })
        .map(|event| process_error_row(&event))
        .collect();
    Ok((rows, scanned.coverage_since))
}

fn row_timestamp(row: &Value) -> Option<DateTime<Utc>> {
    row["ts"].as_str().and_then(|ts| ts.parse().ok())
}

fn process_error_row(event: &Value) -> Value {
    let ts = event.get("timestamp").and_then(Value::as_str).unwrap_or("");
    let target = event.get("target").and_then(Value::as_str).unwrap_or("-");
    let empty_fields = Value::Object(Default::default());
    let fields = event.get("fields").unwrap_or(&empty_fields);
    let job_run = optional_log_field(fields, &["job_run_id", "run_id"]);
    let affiliation = if job_run.is_some() {
        "run"
    } else {
        "unaffiliated"
    };

    json!({
        "ts": ts,
        "source": "process",
        "message": optional_log_field(fields, &["error_message", "error"])
            .map(redact_all).unwrap_or_else(|| {
                if target == "orbit.job.step_finished" {
                    "no failure detail recorded".to_string()
                } else {
                    strip_htmlish(&format_message_html(target, fields))
                }
            }),
        "event_id": optional_log_field(fields, &["event_id"]),
        "job_run": job_run,
        "step": optional_log_field(fields, &["step_id", "step", "activity_id"]),
        "step_index": null,
        "task_id": optional_log_field(fields, &["task_id"]),
        "provider": optional_log_field(fields, &["provider"]),
        "blob_ref": null,
        "target": redact_all(target),
        "affiliation": affiliation,
    })
}

fn optional_log_field<'a>(fields: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| {
        fields
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    })
}

/// Most stderr blobs one `/api/diagnostics/errors` computation reads from
/// disk. Each `cli_invocation_finished` event costs one blob read, so a long
/// history of invocations with no structured error lines would otherwise read
/// thousands of files per poll. Newest invocations are read first, so the cap
/// only ever drops the oldest, and the feed reports the oldest invocation read
/// as the start of its stderr coverage.
pub(super) const MAX_STDERR_BLOBS_PER_REQUEST: usize = 256;

/// Agent-stderr rows newest first, with the timestamp of the oldest invocation
/// read when the blob cap stopped the scan before the window start.
fn agent_stderr_error_rows(
    runtime: &OrbitRuntime,
    range: &DiagnosticsRange,
    limit: usize,
) -> Result<(Vec<Value>, Option<DateTime<Utc>>), orbit_core::OrbitError> {
    // Ancestors can precede the selected window. Keep them for step attribution,
    // but only read stderr blobs belonging to invocations in the requested range.
    let events = stderr_scan_events(runtime, range.until)?;
    let by_id: HashMap<&str, &StderrScanEvent> = events
        .iter()
        .filter_map(|event| event.event_id.as_deref().map(|id| (id, event)))
        .collect();
    let step_index_by_id = step_index_by_id(&events);
    let blob_store = audit_blob_store(runtime);
    let mut rows = Vec::new();
    let mut blobs_read = 0usize;
    let mut oldest_read = None;
    let mut coverage_since = None;
    let mut seen = HashSet::new();
    // `events` is oldest-first so the step index above numbers steps in
    // execution order; the row scan walks newest-first because it stops at
    // `2 * limit` rows and the caller keeps only the newest `limit` of them.
    for event in events.iter().rev() {
        if event.body_kind() != Some("cli_invocation_finished") {
            continue;
        }
        let Some(invoked_at) = event
            .ts
            .as_deref()
            .and_then(|ts| ts.parse::<DateTime<Utc>>().ok())
            .filter(|ts| range.contains(*ts))
        else {
            continue;
        };
        if let Some(id) = event.event_id.as_deref()
            && !seen.insert(id)
        {
            continue;
        }
        let Some(blob_ref) = event.stderr_blob_ref.as_deref() else {
            continue;
        };
        if blobs_read >= MAX_STDERR_BLOBS_PER_REQUEST {
            coverage_since = oldest_read;
            break;
        }
        blobs_read += 1;
        oldest_read = Some(invoked_at);
        let stderr = read_blob_text_best_effort(&blob_store, blob_ref);
        let fallback_ts = event.ts.clone().unwrap_or_default();
        let step = enclosing_step_id_for_event(event, &by_id);
        let step_index = step.as_deref().and_then(|step| {
            step_index_by_id
                .get(&(event.run_id.as_deref().unwrap_or(""), step))
                .copied()
        });
        let parsed = parse_structured_error_lines(&stderr, &fallback_ts);
        let mut messages = Vec::new();
        let mut targets = Vec::new();
        let mut timestamps = Vec::new();
        for line in parsed {
            let Ok(ts) = line.ts.parse::<DateTime<Utc>>() else {
                continue;
            };
            if !range.contains(ts) {
                continue;
            }
            let message = redact_all(&line.message);
            if !messages.contains(&message) {
                messages.push(message);
            }
            if !targets.contains(&line.target) {
                targets.push(redact_all(&line.target));
            }
            timestamps.push(ts);
        }
        if messages.is_empty() {
            continue;
        }
        rows.push(json!({
            "ts": timestamps.into_iter().max().map(|ts| ts.to_rfc3339()).unwrap_or(fallback_ts),
            "source": "agent-stderr",
            "message": messages.join("\n"),
            "job_run": event.run_id.as_deref(),
            "step": step,
            "step_index": step_index,
            "task_id": event.task_id.as_deref(),
            "provider": event.provider.as_deref(),
            "blob_ref": blob_ref,
            "event_id": event.event_id.as_deref(),
            "target": targets.join(", "),
        }));
        if rows.len() >= limit.saturating_mul(2) {
            break;
        }
    }
    Ok((rows, coverage_since))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ParsedErrorLine {
    pub ts: String,
    pub target: String,
    pub message: String,
}

fn parse_structured_error_lines(stderr: &str, fallback_ts: &str) -> Vec<ParsedErrorLine> {
    stderr
        .lines()
        .filter_map(|line| parse_structured_error_line(line, fallback_ts))
        .collect()
}

pub(super) fn parse_structured_error_line(
    line: &str,
    fallback_ts: &str,
) -> Option<ParsedErrorLine> {
    let trimmed = line.trim();
    let (ts, rest) = if let Some((head, tail)) = trimmed.split_once(" ERROR ") {
        if DateTime::parse_from_rfc3339(head).is_ok() {
            (head.to_string(), tail)
        } else {
            (fallback_ts.to_string(), trimmed.strip_prefix("ERROR ")?)
        }
    } else {
        (fallback_ts.to_string(), trimmed.strip_prefix("ERROR ")?)
    };
    let (target, message) = rest.split_once(": ")?;
    let target = target.trim();
    let message = message.trim();
    if target.is_empty() || message.is_empty() {
        return None;
    }
    Some(ParsedErrorLine {
        ts,
        target: target.to_string(),
        message: message.to_string(),
    })
}

fn step_index_by_id(events: &[StderrScanEvent]) -> HashMap<(&str, &str), u32> {
    let mut result = HashMap::new();
    let mut next_by_run = HashMap::new();
    for event in events {
        if event.body_kind() != Some("step_started") {
            continue;
        }
        let Some(step_id) = event.step_id() else {
            continue;
        };
        let run = event.run_id.as_deref().unwrap_or("");
        let next = next_by_run.entry(run).or_insert(0);
        result.entry((run, step_id)).or_insert_with(|| {
            let index = *next;
            *next += 1;
            index
        });
    }
    result
}

/// Newest v2 envelope events the Errors feed scans for agent stderr.
const STDERR_SCAN_EVENTS: usize = 50_000;

/// Store rows read per page of that scan.
const STDERR_SCAN_PAGE: usize = 5_000;

/// The fields the Errors feed reads from one v2 envelope event. A full JSON
/// tree per event made the 50,000-event scan the largest allocation of a
/// dashboard poll; these fields take a small fraction of that.
#[derive(Deserialize)]
struct StderrScanEvent {
    #[serde(default, deserialize_with = "string_field")]
    event_id: Option<String>,
    #[serde(default, deserialize_with = "string_field")]
    parent_event_id: Option<String>,
    #[serde(default, deserialize_with = "string_field")]
    body_kind: Option<String>,
    #[serde(default, deserialize_with = "string_field")]
    step_id: Option<String>,
    #[serde(default, deserialize_with = "string_field")]
    run_id: Option<String>,
    #[serde(default, deserialize_with = "string_field")]
    ts: Option<String>,
    #[serde(default, deserialize_with = "string_field")]
    stderr_blob_ref: Option<String>,
    #[serde(default, deserialize_with = "string_field")]
    task_id: Option<String>,
    #[serde(default, deserialize_with = "string_field")]
    provider: Option<String>,
}

impl EnvelopeLinks for StderrScanEvent {
    fn step_id(&self) -> Option<&str> {
        self.step_id.as_deref()
    }

    fn parent_event_id(&self) -> Option<&str> {
        self.parent_event_id.as_deref()
    }

    fn body_kind(&self) -> Option<&str> {
        self.body_kind.as_deref()
    }
}

/// A string payload field, or `None` for any other JSON type, matching the
/// `Value::as_str` reads this view replaced.
fn string_field<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Ok(match Value::deserialize(deserializer)? {
        Value::String(value) => Some(value),
        _ => None,
    })
}

/// The newest [`STDERR_SCAN_EVENTS`] v2 envelope events up to `until`, oldest
/// first. Pages through the store so one page of rows is alive at a time.
fn stderr_scan_events(
    runtime: &OrbitRuntime,
    until: DateTime<Utc>,
) -> Result<Vec<StderrScanEvent>, orbit_core::OrbitError> {
    let mut events = Vec::new();
    let mut scanned = 0;
    while scanned < STDERR_SCAN_EVENTS {
        let requested = STDERR_SCAN_PAGE.min(STDERR_SCAN_EVENTS - scanned);
        let page = OrbitRuntime::list_v2_audit_events(
            runtime,
            V2AuditEventFilter {
                workspace_id: String::new(),
                until: Some(until),
                source: Some("v2_envelope".to_string()),
                limit: Some(requested),
                offset: Some(scanned),
                ..Default::default()
            },
        )?;
        let fetched = page.len();
        scanned += fetched;
        events.extend(
            page.into_iter()
                .filter_map(|row| serde_json::from_str(&row.payload_json).ok()),
        );
        if fetched < requested {
            break;
        }
    }
    events.reverse();
    Ok(events)
}

fn strip_htmlish(raw: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in raw.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
}
