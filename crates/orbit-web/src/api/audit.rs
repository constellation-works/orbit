//! Audit event listing and summary tile aggregation.

use std::collections::BTreeMap;
use std::str::FromStr;

use crate::state::Ws;
use axum::extract::Query;
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Duration, Utc};
use orbit_core::application::job::JobRunListParams;
use orbit_core::{
    AuditEventFilter, AuditEventStatus, AuditToolAggregate, FailureClass, FailureIncidentQuery,
    FailureIncidentReport, JOB_RUN_LIFECYCLE_LABEL, JobRunState, LIFECYCLE_DIAGNOSTIC_LABEL,
    OrbitError, OrbitRuntime, is_failure_only_diagnostic_surface,
};
use orbit_types::tool::{McpCapability, McpTransport};
use serde_json::{Value, json};

use super::denials::{
    collect_denial_rows, denials_by_reason_summary, denials_by_tool_summary, scan_v2_loop_denials,
};
use super::incidents::{ROLLUP_SCAN_LIMIT, failure_category_summaries};
use super::{
    AuditQuery, AuditSummaryQuery, DEFAULT_SUMMARY_WINDOW, HISTORY_DEFAULT_LIMIT,
    HISTORY_MAX_LIMIT, bad_request, bounded_limit, map_runtime_error, server_error,
    truncate_to_hour,
};
use crate::parse::parse_since;
use crate::projections::audit_event_to_json;

/// Default header-tile alert threshold for the denials counter. Surfaced via
/// `?denial_threshold=` and echoed back in the response so the dashboard can
/// switch the tile to alert state without a second round-trip.
const DEFAULT_DENIAL_THRESHOLD: i64 = 10;

pub(super) async fn list_audit(Ws(runtime): Ws, Query(q): Query<AuditQuery>) -> Response {
    let since = match q.since.as_deref() {
        Some(raw) => match parse_since(raw) {
            Ok(ts) => Some(ts),
            Err(e) => return map_runtime_error(e),
        },
        None => None,
    };

    let status = match q.status.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => match AuditEventStatus::from_str(raw) {
            Ok(s) => Some(s),
            Err(msg) => return bad_request(msg),
        },
        None => None,
    };

    let limit = bounded_limit(q.limit, HISTORY_DEFAULT_LIMIT);
    let offset = q.offset.unwrap_or(0);
    let tool = q.tool.filter(|s| !s.is_empty());
    let role = q.role.filter(|s| !s.is_empty());
    let transport = match q
        .transport
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => match value.parse::<McpTransport>() {
            Ok(value) => Some(value),
            Err(message) => return bad_request(message),
        },
        None => None,
    };
    let capability = match q
        .capability
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => match value.parse::<McpCapability>() {
            Ok(value) => Some(value),
            Err(message) => return bad_request(message),
        },
        None => None,
    };

    let mut filter = AuditEventFilter {
        since,
        tool_name: tool,
        target_type: None,
        status,
        role,
        workspace_id: q.workspace_id.filter(|value| !value.is_empty()),
        caller_machine_id: q.caller_machine.filter(|value| !value.is_empty()),
        process_machine_id: q.process_machine.filter(|value| !value.is_empty()),
        transport,
        capability,
        origin_session_id: q.origin_session.filter(|value| !value.is_empty()),
        mcp_call_id: q.mcp_call.filter(|value| !value.is_empty()),
        job_run_id: q.job_run_id.filter(|value| !value.is_empty()),
        lease_id: q.lease.filter(|value| !value.is_empty()),
        limit,
        offset,
    };

    let post_filter = AuditPostFilter {
        execution_id: q
            .execution_id
            .as_deref()
            .or(q.run_id.as_deref())
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        profile: q
            .profile
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        needle: q
            .q
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_lowercase),
    };

    let page = if post_filter.is_empty() {
        // Every requested predicate has a column, so the page is exactly the
        // SQL window: no prefetch, no Rust-side slicing.
        match runtime.list_audit_events_filtered(&filter) {
            Ok(events) => events,
            Err(e) => return server_error(e),
        }
    } else {
        match scan_audit_page(&runtime, &mut filter, &post_filter, offset, limit) {
            Ok(events) => events,
            Err(e) => return server_error(e),
        }
    };

    let page: Vec<Value> = page.iter().map(audit_event_to_json).collect();
    Json(Value::Array(page)).into_response()
}

/// Predicates the SQLite schema has no column for, applied to each fetched
/// row in Rust.
struct AuditPostFilter {
    execution_id: Option<String>,
    profile: Option<String>,
    /// Lowercased free-text needle.
    needle: Option<String>,
}

impl AuditPostFilter {
    fn is_empty(&self) -> bool {
        self.execution_id.is_none() && self.profile.is_none() && self.needle.is_none()
    }

    fn matches(&self, e: &orbit_core::AuditEvent) -> bool {
        if let Some(eid) = self.execution_id.as_deref()
            && e.execution_id != eid
        {
            return false;
        }
        if let Some(profile) = self.profile.as_deref()
            && !arguments_json_matches_profile(e.arguments_json.as_deref(), profile)
        {
            return false;
        }
        if let Some(needle) = self.needle.as_deref() {
            let haystacks = [
                e.command.as_str(),
                e.subcommand.as_deref().unwrap_or(""),
                e.tool_name.as_deref().unwrap_or(""),
                e.target_id.as_deref().unwrap_or(""),
                e.target_type.as_deref().unwrap_or(""),
                e.role.as_str(),
                e.error_message.as_deref().unwrap_or(""),
            ];
            if !haystacks.iter().any(|h| h.to_lowercase().contains(needle)) {
                return false;
            }
        }
        true
    }
}

/// Rows a single `/api/audit` request may pull from SQLite while satisfying
/// a Rust-side predicate. Bounds the cost of a needle that matches nothing
/// in a long history; a page that hits the cap simply comes back short.
const AUDIT_POST_FILTER_SCAN_CAP: usize = 10_000;

/// Walk the SQL window in `HISTORY_MAX_LIMIT` batches, keeping rows that pass
/// `post_filter`, until `offset + limit` matches are in hand, the store runs
/// dry, or the scan cap is reached. `filter.limit`/`filter.offset` are used
/// as scratch for the batch window.
fn scan_audit_page(
    runtime: &OrbitRuntime,
    filter: &mut AuditEventFilter,
    post_filter: &AuditPostFilter,
    offset: usize,
    limit: usize,
) -> Result<Vec<orbit_core::AuditEvent>, OrbitError> {
    let wanted = offset.saturating_add(limit);
    let mut matched = Vec::new();
    let mut scanned = 0usize;
    filter.limit = HISTORY_MAX_LIMIT;
    filter.offset = 0;
    while matched.len() < wanted && scanned < AUDIT_POST_FILTER_SCAN_CAP {
        let batch = runtime.list_audit_events_filtered(filter)?;
        let fetched = batch.len();
        scanned += fetched;
        matched.extend(batch.into_iter().filter(|e| post_filter.matches(e)));
        if fetched < HISTORY_MAX_LIMIT {
            break;
        }
        filter.offset += fetched;
    }
    Ok(matched.into_iter().skip(offset).take(limit).collect())
}

/// Best-effort match of a stringified `arguments_json` payload against a
/// requested fsProfile name. Looks for any of the conventional keys
/// (`fsProfile`, `fs_profile`, `profile`) at the top level of the parsed
/// object. Returns `false` for malformed or empty payloads — the SQLite schema
/// has no profile column, so absence cannot be distinguished from mismatch.
fn arguments_json_matches_profile(raw: Option<&str>, expected: &str) -> bool {
    let Some(raw) = raw else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    const KEYS: &[&str] = &["fsProfile", "fs_profile", "profile"];
    let Some(obj) = value.as_object() else {
        return false;
    };
    for key in KEYS {
        if let Some(Value::String(found)) = obj.get(*key)
            && found == expected
        {
            return true;
        }
    }
    false
}

pub(super) async fn audit_summary(Ws(runtime): Ws, Query(q): Query<AuditSummaryQuery>) -> Response {
    let raw_since = q.since.as_deref().unwrap_or(DEFAULT_SUMMARY_WINDOW);
    let since = match parse_since(raw_since) {
        Ok(ts) => ts,
        Err(e) => return map_runtime_error(e),
    };
    let denial_threshold = q.denial_threshold.unwrap_or(DEFAULT_DENIAL_THRESHOLD);
    let raw_since_owned = raw_since.to_string();

    let runtime_clone = runtime.clone();
    let bundle = match tokio::task::spawn_blocking(move || {
        compute_audit_summary_bundle(&runtime_clone, since)
    })
    .await
    {
        Ok(Ok(b)) => b,
        Ok(Err(e)) => return server_error(e),
        Err(join_err) => {
            return server_error(OrbitError::Execution(format!(
                "audit summary aggregation panicked: {join_err}"
            )));
        }
    };

    let sparkline = build_sparkline(since, &bundle.buckets);
    let denials = bundle.sql_denied + bundle.v2_denials;

    Json(json!({
        "events": bundle.total,
        "denials": denials,
        "denials_sql": bundle.sql_denied,
        "denials_v2": bundle.v2_denials,
        // ORB-10871: raw failed rows and grouped incidents are reported as two
        // separate numbers over the same window, so neither is mistaken for
        // the other. `failed_events` is the forensic count; `failure_incidents`
        // is how many distinct problems those rows represent.
        "failed_events": bundle.failed_events,
        "failure_incidents": bundle.failure_incidents,
        "failure_incidents_by_class": bundle.failure_incidents_by_class,
        "failed_events_by_class": bundle.failed_events_by_class,
        "affected_runs_by_class": bundle.affected_runs_by_class,
        "failure_categories": bundle.failure_categories,
        "affected_run_count": bundle.affected_run_count,
        "job_run_lifecycle_failures": bundle.job_run_lifecycle_failures,
        "job_run_lifecycle_incidents": bundle.job_run_lifecycle_incidents,
        "job_run_lifecycle_label": JOB_RUN_LIFECYCLE_LABEL,
        "lifecycle_diagnostic_events": bundle.lifecycle_diagnostic_events,
        "lifecycle_diagnostic_incidents": bundle.lifecycle_diagnostic_incidents,
        "lifecycle_diagnostic_affected_run_count": bundle.lifecycle_diagnostic_affected_run_count,
        "lifecycle_diagnostic_label": LIFECYCLE_DIAGNOSTIC_LABEL,
        "failed_runs": bundle.failed_runs,
        "active_long_runs": bundle.active_long_runs,
        "sparkline": sparkline,
        "denial_threshold": denial_threshold,
        "since": since.to_rfc3339(),
        "window": raw_since_owned,
        "failures_by_tool": bundle.failures_by_tool,
        "duration_by_tool": bundle.duration_by_tool,
        "failure_rate_by_tool": bundle.failure_rate_by_tool,
        "role_split": bundle.role_split,
        "actor_split": bundle.actor_split,
        "attribution_split": bundle.attribution_split,
        "mcp_vs_cli_split": bundle.mcp_vs_cli_split,
        "denials_by_tool": bundle.denials_by_tool,
        "denials_by_reason": bundle.denials_by_reason,
    }))
    .into_response()
}

struct AuditSummaryBundle {
    total: i64,
    sql_denied: i64,
    v2_denials: i64,
    failed_events: u64,
    failure_incidents: u64,
    failure_incidents_by_class: BTreeMap<String, u64>,
    failed_events_by_class: BTreeMap<String, u64>,
    affected_runs_by_class: BTreeMap<String, u64>,
    failure_categories: Value,
    affected_run_count: u64,
    job_run_lifecycle_failures: u64,
    job_run_lifecycle_incidents: u64,
    lifecycle_diagnostic_events: u64,
    lifecycle_diagnostic_incidents: u64,
    lifecycle_diagnostic_affected_run_count: u64,
    failed_runs: i64,
    active_long_runs: i64,
    buckets: Vec<(String, i64)>,
    failures_by_tool: Vec<Value>,
    duration_by_tool: Vec<Value>,
    failure_rate_by_tool: Vec<Value>,
    role_split: Vec<Value>,
    /// Canonical per-actor split [ORB-10888]. Unlike `role_split`, one agent
    /// appears once regardless of the granularity its label was recorded at,
    /// and `kind` says whether a row is a real agent at all.
    actor_split: Vec<Value>,
    /// Tool calls split by how each row's identity was established
    /// [ORB-10890]. Every row carries its own `attribution`, so a consumer
    /// cannot render a self-reported count as an authenticated one; the
    /// buckets are disjoint, so summing them is the combined denominator.
    attribution_split: Vec<Value>,
    mcp_vs_cli_split: Value,
    denials_by_tool: Value,
    denials_by_reason: Value,
}

/// Heavy synchronous portion of `audit_summary`. Bundled into a single
/// function so the caller can move it onto a `spawn_blocking` thread —
/// every dependency below issues sync SQLite I/O.
fn compute_audit_summary_bundle(
    runtime: &OrbitRuntime,
    since: DateTime<Utc>,
) -> Result<AuditSummaryBundle, OrbitError> {
    let stats = runtime.audit_event_stats(Some(since), None)?;
    let total = stats.total;
    let sql_denied = stats.denied_count;

    let v2_denials = scan_v2_loop_denials(runtime, Some(since), None, None)?.len() as i64;

    // ORB-10871: the same window, grouped. Reported next to `total` so the
    // header tiles can state both counts with their denominators.
    let incidents = runtime.audit_failure_incidents(&FailureIncidentQuery {
        since: Some(since),
        max_events: ROLLUP_SCAN_LIMIT,
        ..Default::default()
    })?;

    let failed_runs = count_failed_runs(runtime, since)?;
    let active_long_runs = count_active_long_runs(runtime, since)?;
    let buckets = runtime.audit_event_hourly_buckets(&since)?;

    let tool_aggs = runtime.audit_event_aggregates_by_tool(&since)?;
    let role_aggs = runtime.audit_event_aggregates_by_role(&since)?;
    let actor_aggs = runtime.audit_event_aggregates_by_actor(&since)?;

    let unexpected_by_tool = raw_failure_counts_by_tool(&incidents, FailureClass::Unexpected);
    let mut failures_vec: Vec<_> = unexpected_by_tool
        .iter()
        .map(|(tool, count)| {
            json!({
                "tool": tool,
                "count": count,
                "class": FailureClass::Unexpected.as_str(),
            })
        })
        .collect();
    failures_vec.sort_by_key(|v| std::cmp::Reverse(v["count"].as_i64().unwrap_or(0)));
    failures_vec.truncate(8);

    let mut by_avg: Vec<&AuditToolAggregate> = tool_aggs.iter().collect();
    by_avg.sort_by(|a, b| {
        b.avg_duration_ms
            .partial_cmp(&a.avg_duration_ms)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut duration_vec = Vec::with_capacity(8);
    for t in by_avg.iter().take(8) {
        // `"unknown"` is the synthetic bucket for rows with NULL `tool_name`;
        // a `tool_name = 'unknown'` query would miss them entirely, so we
        // pull NULL-tool durations through a dedicated path.
        let p95 = if t.tool_name == "unknown" {
            runtime
                .audit_event_durations_null_tool(&since)
                .map(|d| orbit_core::application::audit_event::compute_p95(&d))
                .unwrap_or(0)
        } else {
            runtime
                .audit_event_stats(Some(since), Some(t.tool_name.clone()))
                .map(|s| s.p95_duration_ms)
                .unwrap_or(0)
        };
        duration_vec.push(json!({
            "tool": t.tool_name,
            "count": t.total,
            "avg": t.avg_duration_ms,
            "p95": p95,
        }));
    }

    let mut rate_vec: Vec<_> = tool_aggs
        .iter()
        .filter_map(|t| {
            let unexpected_failures = unexpected_by_tool.get(&t.tool_name).copied().unwrap_or(0);
            let comparison_population = t.successes + unexpected_failures;
            let is_callable = t.mcp_total + t.cli_total > 0;
            if !is_named_tool(&t.tool_name)
                || is_failure_only_diagnostic_surface(&t.tool_name)
                || !is_callable
                || t.successes == 0
                || unexpected_failures == 0
                || comparison_population < 5
            {
                return None;
            }
            let rate = unexpected_failures as f64 / comparison_population as f64;
            Some(json!({
                "tool": t.tool_name,
                "rate": rate,
                "failures": unexpected_failures,
                "successes": t.successes,
                "total": comparison_population,
                "denominator": "successful + unexpected failed calls",
            }))
        })
        .collect();
    rate_vec.sort_by(|a, b| {
        b["rate"]
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&a["rate"].as_f64().unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    rate_vec.truncate(8);

    let role_vec: Vec<_> = role_aggs
        .iter()
        .map(|r| {
            json!({
                "label": r.role,
                "count": r.total,
                "mcp": r.mcp,
                "cli": r.cli,
                "other": r.other,
                "no_subcommand": r.no_subcommand,
            })
        })
        .collect();

    let actor_vec: Vec<_> = actor_aggs
        .iter()
        .map(|a| {
            json!({
                "label": a.actor,
                "kind": a.kind,
                "vendor": a.vendor,
                "family": a.family,
                "count": a.total,
                "mcp": a.mcp,
                "cli": a.cli,
            })
        })
        .collect();

    let attribution_vec: Vec<_> = runtime
        .audit_tool_call_counts_by_attribution(Some(&since))?
        .iter()
        .map(|a| {
            json!({
                "label": a.actor,
                "attribution": a.attribution,
                // Redundant with `attribution`, but a chart legend that only
                // reads `label` still says which half of the split it is in.
                "verified": a.attribution.is_authenticated(),
                "count": a.total,
                "failed": a.failed,
                "mcp": a.mcp,
                "cli": a.cli,
            })
        })
        .collect();

    let mcp_count: i64 = role_aggs.iter().map(|r| r.mcp).sum();
    let cli_count: i64 = role_aggs.iter().map(|r| r.cli).sum();
    let mcp_vs_cli_split = json!([
        {"label": "mcp", "count": mcp_count},
        {"label": "cli", "count": cli_count},
    ]);

    let denial_rows = collect_denial_rows(runtime, Some(since), None, None)?;
    let denials_by_tool = denials_by_tool_summary(&denial_rows, 8);
    let denials_by_reason = denials_by_reason_summary(&denial_rows, 8);
    let failure_categories = failure_category_summaries(&incidents);

    Ok(AuditSummaryBundle {
        total,
        sql_denied,
        v2_denials,
        failed_events: incidents.raw_failed_events,
        failure_incidents: incidents.incident_count(),
        failure_incidents_by_class: incidents.incidents_by_class,
        failed_events_by_class: incidents.raw_events_by_class,
        affected_runs_by_class: incidents.affected_runs_by_class,
        failure_categories,
        affected_run_count: incidents.affected_run_count,
        job_run_lifecycle_failures: incidents.job_run_lifecycle_events,
        job_run_lifecycle_incidents: incidents.job_run_lifecycle_incidents,
        lifecycle_diagnostic_events: incidents.lifecycle_diagnostic_events,
        lifecycle_diagnostic_incidents: incidents.lifecycle_diagnostic_incidents,
        lifecycle_diagnostic_affected_run_count: incidents.lifecycle_diagnostic_affected_run_count,
        failed_runs,
        active_long_runs,
        buckets,
        failures_by_tool: failures_vec,
        duration_by_tool: duration_vec,
        failure_rate_by_tool: rate_vec,
        role_split: role_vec,
        actor_split: actor_vec,
        attribution_split: attribution_vec,
        mcp_vs_cli_split,
        denials_by_tool,
        denials_by_reason,
    })
}

/// Counts raw incident evidence by tool for one class. This deliberately uses
/// the existing incident classifier instead of maintaining a second list of
/// expected/diagnostic message rules in the dashboard API.
fn raw_failure_counts_by_tool(
    report: &FailureIncidentReport,
    class: FailureClass,
) -> BTreeMap<String, i64> {
    let mut counts = BTreeMap::new();
    for event in report
        .incidents
        .iter()
        .filter(|incident| incident.class == class)
        .flat_map(|incident| &incident.events)
    {
        let Some(tool) = event
            .tool_name
            .as_deref()
            .filter(|tool| is_named_tool(tool) && !is_failure_only_diagnostic_surface(tool))
        else {
            continue;
        };
        *counts.entry(tool.to_string()).or_insert(0) += 1;
    }
    counts
}

/// Builds a contiguous hourly sparkline covering `[truncate_to_hour(since), now]`,
/// zero-filling hours not present in `buckets`. Always returns at least 24
/// buckets so the UI can render a stable baseline width even on a fresh
/// workspace.
fn build_sparkline(since: DateTime<Utc>, buckets: &[(String, i64)]) -> Vec<Value> {
    let mut by_bucket: BTreeMap<String, i64> = BTreeMap::new();
    for (ts, count) in buckets {
        by_bucket.insert(ts.clone(), *count);
    }
    let now = Utc::now();
    let start = truncate_to_hour(since.min(now));
    let end = truncate_to_hour(now);
    let mut out = Vec::new();
    let mut cursor = start;
    while cursor <= end {
        let key = cursor.format("%Y-%m-%dT%H:00:00Z").to_string();
        let count = by_bucket.get(&key).copied().unwrap_or(0);
        out.push(json!({ "ts": key, "count": count }));
        cursor += Duration::hours(1);
    }
    while out.len() < 24 {
        let earliest = out
            .first()
            .and_then(|v| v.get("ts").and_then(Value::as_str))
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or(end);
        let prev = earliest - Duration::hours(1);
        let key = prev.format("%Y-%m-%dT%H:00:00Z").to_string();
        out.insert(0, json!({ "ts": key, "count": 0 }));
    }
    out
}

fn count_failed_runs(
    runtime: &OrbitRuntime,
    since: DateTime<Utc>,
) -> Result<i64, orbit_core::OrbitError> {
    let mut total: u64 = 0;
    for state in [
        JobRunState::Failed,
        JobRunState::Timeout,
        JobRunState::Interrupted,
    ] {
        total += runtime.count_job_runs(JobRunListParams {
            job_id: None,
            state: Some(state),
            terminal_only: false,
            since: Some(since),
            limit: None,
            ..Default::default()
        })?;
    }
    Ok(i64::try_from(total).unwrap_or(i64::MAX))
}

/// Counts running runs whose start time is older than the 95th percentile of
/// finished-run wall-clock durations within the same window. We use run-level
/// `duration_ms` as a proxy for the AC's "finished step" series — load-bearing
/// the same per-run signal without paying the O(steps) file-read cost. Faithful
/// to the spec's intent (flag stuck activity) and within the 500ms budget.
fn count_active_long_runs(
    runtime: &OrbitRuntime,
    since: DateTime<Utc>,
) -> Result<i64, orbit_core::OrbitError> {
    // Every finished run in the window, as durations only: the baseline is
    // a percentile, so a capped page of hydrated rows would both drift low
    // and cost the step reads it never looks at.
    let mut finished_durations: Vec<i64> = runtime
        .list_job_run_durations(JobRunListParams {
            job_id: None,
            state: None,
            terminal_only: true,
            since: Some(since),
            limit: None,
            ..Default::default()
        })?
        .into_iter()
        .map(|d| i64::try_from(d).unwrap_or(i64::MAX))
        .collect();

    if finished_durations.is_empty() {
        return Ok(0);
    }
    finished_durations.sort_unstable();
    let idx = ((finished_durations.len() as f64) * 0.95).ceil() as usize;
    let idx = idx.min(finished_durations.len()).saturating_sub(1);
    let p95_ms = finished_durations[idx];

    let running = runtime.list_job_runs(JobRunListParams {
        job_id: None,
        state: Some(JobRunState::Running),
        terminal_only: false,
        since: None,
        limit: None,
        ..Default::default()
    })?;

    let now = Utc::now();
    let mut count: i64 = 0;
    for r in running {
        let started = r.started_at.unwrap_or(r.created_at);
        let elapsed = now.signed_duration_since(started).num_milliseconds().max(0);
        if elapsed > p95_ms {
            count += 1;
        }
    }
    Ok(count)
}

/// SQL aggregates fold NULL `tool_name` into `"unknown"`. That bucket is not
/// a tool: those rows are job-run lifecycle events and must not enter tool
/// denominators or rates.
fn is_named_tool(name: &str) -> bool {
    let trimmed = name.trim();
    !trimmed.is_empty() && trimmed != "unknown"
}
