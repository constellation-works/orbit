//! Audit event listing and summary tile aggregation.

use std::collections::{BTreeMap, HashSet};
use std::str::FromStr;

use crate::state::{DashboardState, Ws};
use axum::extract::{Query, State};
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

use super::denials::{collect_denial_rows, denials_by_reason_summary, denials_by_tool_summary};
use super::incidents::{ROLLUP_SCAN_LIMIT, failure_category_summaries};
use super::jobs::FAILED_RUN_STATES;
use super::{
    AuditQuery, AuditSummaryQuery, DEFAULT_SUMMARY_WINDOW, HISTORY_DEFAULT_LIMIT,
    HISTORY_MAX_LIMIT, bad_request, blocking, bounded_limit, map_runtime_error, server_error,
    truncate_to_hour,
};
use crate::parse::{parse_duration_seconds, parse_since};
use crate::projections::audit_event_to_json;
use crate::runtime_memo::AUDIT_SUMMARY_TTL;

/// Default header-tile alert threshold for the denials counter. Surfaced via
/// `?denial_threshold=` and echoed back in the response so the dashboard can
/// switch the tile to alert state without a second round-trip.
const DEFAULT_DENIAL_THRESHOLD: i64 = 10;

/// Largest `?offset=` accepted by `GET /audit`. SQLite walks and discards
/// every skipped row, so an unbounded offset is an unbounded scan. Rejecting
/// (rather than clamping) keeps a too-deep page from silently returning rows
/// from the wrong position.
const AUDIT_MAX_OFFSET: usize = 100_000;

/// Longest `GET /audit/summary` window. Dashboard selections are `1h`, `24h`,
/// `7d`, and `30d` (`all` falls back to 24h). A wider `since` is rejected.
const MAX_SUMMARY_WINDOW_DAYS: usize = 30;

/// Inclusive UTC hours in [`MAX_SUMMARY_WINDOW_DAYS`]: 720 elapsed hours plus
/// the truncated start hour. [`build_sparkline`] never emits more than this.
const MAX_SUMMARY_SPARKLINE_BUCKETS: usize = MAX_SUMMARY_WINDOW_DAYS * 24 + 1;

/// The incident rollup itself is bounded to this many source rows, so an
/// exact incident drilldown never needs more IDs than this.
const MAX_AUDIT_EVENT_IDS: usize = 10_000;

pub(super) async fn list_audit(Ws(runtime): Ws, Query(q): Query<AuditQuery>) -> Response {
    let event_ids = match q.ids.as_deref() {
        Some(raw) => match parse_audit_event_ids(raw) {
            Ok(ids) => Some(ids),
            Err(message) => return bad_request(message),
        },
        None => None,
    };

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
    if offset > AUDIT_MAX_OFFSET {
        return bad_request(format!(
            "offset must be <= {AUDIT_MAX_OFFSET}; got {offset}"
        ));
    }
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

    match blocking("audit list", move || {
        let events = if let Some(ids) = event_ids.as_deref() {
            runtime.list_audit_events_by_ids(ids, filter.workspace_id.as_deref())?
        } else if post_filter.is_empty() {
            // Every requested predicate has a column, so the page is exactly the
            // SQL window: no prefetch, no Rust-side slicing.
            runtime.list_audit_events_filtered(&filter)?
        } else {
            scan_audit_page(&runtime, &mut filter, &post_filter, offset, limit)?
        };
        Ok(events)
    })
    .await
    {
        Ok(page) => {
            let page: Vec<Value> = page.iter().map(audit_event_to_json).collect();
            Json(Value::Array(page)).into_response()
        }
        Err(response) => *response,
    }
}

fn parse_audit_event_ids(raw: &str) -> Result<Vec<i64>, String> {
    if raw.split(',').count() > MAX_AUDIT_EVENT_IDS {
        return Err(format!(
            "ids must contain at most {MAX_AUDIT_EVENT_IDS} values"
        ));
    }

    let mut ids = Vec::new();
    let mut seen = HashSet::new();
    for raw_id in raw.split(',') {
        let id = raw_id
            .parse::<i64>()
            .map_err(|_| "ids must be comma-separated positive audit row IDs".to_string())?;
        if id <= 0 {
            return Err("ids must be comma-separated positive audit row IDs".to_string());
        }
        if seen.insert(id) {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        return Err("ids must include at least one audit row ID".to_string());
    }
    Ok(ids)
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
    let mut to_skip = offset;
    let mut page = Vec::new();
    let mut scanned = 0usize;
    filter.limit = HISTORY_MAX_LIMIT;
    filter.offset = 0;
    while page.len() < limit && scanned < AUDIT_POST_FILTER_SCAN_CAP {
        let batch = OrbitRuntime::list_audit_events_filtered(runtime, filter)?;
        let fetched = batch.len();
        scanned += fetched;
        // Keep only the requested page; matches before `offset` are counted,
        // not buffered.
        for event in batch.into_iter().filter(|e| post_filter.matches(e)) {
            if to_skip > 0 {
                to_skip -= 1;
            } else if page.len() < limit {
                page.push(event);
            }
        }
        if fetched < HISTORY_MAX_LIMIT {
            break;
        }
        filter.offset += fetched;
    }
    Ok(page)
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

pub(super) async fn audit_summary(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Query(q): Query<AuditSummaryQuery>,
) -> Response {
    let raw_since = q.since.as_deref().unwrap_or(DEFAULT_SUMMARY_WINDOW);
    // One clock for the parsed cutoff and the sparkline, so a 30-day window
    // is exactly [`MAX_SUMMARY_SPARKLINE_BUCKETS`] buckets.
    let now = Utc::now();
    let since = match summary_since(raw_since, now) {
        Ok(ts) => ts,
        Err(e) => return map_runtime_error(e),
    };
    let bucket_count = sparkline_bucket_count(since, now);
    if bucket_count > MAX_SUMMARY_SPARKLINE_BUCKETS {
        return bad_request(format!(
            "audit summary since '{raw_since}' covers {bucket_count} hourly buckets; the maximum is {MAX_SUMMARY_SPARKLINE_BUCKETS} ({MAX_SUMMARY_WINDOW_DAYS} days)"
        ));
    }
    let denial_threshold = q.denial_threshold.unwrap_or(DEFAULT_DENIAL_THRESHOLD);
    let window_json = raw_since.to_string();
    let runtime_for_compute = runtime.clone();

    let cached = match state
        .audit_summary_memo()
        .get_or_compute(
            &runtime,
            raw_since.to_string(),
            AUDIT_SUMMARY_TTL,
            move || {
                let bundle = compute_audit_summary_bundle(&runtime_for_compute, since)?;
                Ok(summary_payload(&bundle, since, now, &window_json))
            },
        )
        .await
    {
        Ok(body) => body,
        Err(e) => return server_error(e),
    };

    let mut body = (*cached).clone();
    if let Some(obj) = body.as_object_mut() {
        obj.insert("denial_threshold".to_string(), json!(denial_threshold));
    }
    Json(body).into_response()
}

struct AuditSummaryBundle {
    total: i64,
    sql_denied: i64,
    v2_denials: i64,
    failed_events: u64,
    failure_incidents: u64,
    failure_incidents_truncated: bool,
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
    /// Raw `status=failure` over callable tool calls (`run` + `run-mcp`).
    /// Distinct from `failure_rate_by_tool`, which is unexpected-only with a
    /// successful + unexpected-failed denominator and a sample-size floor.
    tool_call_failure_rate: Value,
    tool_call_failures_by_tool: Vec<Value>,
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

/// Stable JSON fields for a computed bundle. `denial_threshold` is request
/// echo, not part of the scan, so the handler stamps it after the memo hit.
fn summary_payload(
    bundle: &AuditSummaryBundle,
    since: DateTime<Utc>,
    now: DateTime<Utc>,
    window: &str,
) -> Value {
    let sparkline = build_sparkline(since, now, &bundle.buckets);
    let denials = bundle.sql_denied + bundle.v2_denials;
    json!({
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
        // All counts derived from the incident scan share this coverage,
        // while `events` is the uncapped SQL total for the window.
        "failure_incidents_truncated": bundle.failure_incidents_truncated,
        "failure_incidents_scan_limit": ROLLUP_SCAN_LIMIT,
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
        "since": since.to_rfc3339(),
        "window": window,
        "failures_by_tool": bundle.failures_by_tool,
        "duration_by_tool": bundle.duration_by_tool,
        "failure_rate_by_tool": bundle.failure_rate_by_tool,
        "tool_call_failure_rate": bundle.tool_call_failure_rate,
        "tool_call_failures_by_tool": bundle.tool_call_failures_by_tool,
        "role_split": bundle.role_split,
        "actor_split": bundle.actor_split,
        "attribution_split": bundle.attribution_split,
        "mcp_vs_cli_split": bundle.mcp_vs_cli_split,
        "denials_by_tool": bundle.denials_by_tool,
        "denials_by_reason": bundle.denials_by_reason,
    })
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
    let policy = runtime.audit_policy_denial_stats(Some(&since))?;
    let sql_denied = policy.sql_denied;
    let v2_denials = policy.v2_denied;

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

    let mut by_avg: Vec<&AuditToolAggregate> = tool_aggs
        .iter()
        .filter(|tool| is_named_tool(&tool.tool_name))
        .collect();
    by_avg.sort_by(|a, b| {
        b.avg_duration_ms
            .partial_cmp(&a.avg_duration_ms)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut duration_vec = Vec::with_capacity(8);
    for t in by_avg.iter().take(8) {
        let p95 = runtime
            .audit_event_stats(Some(since), Some(t.tool_name.clone()))
            .map(|s| s.p95_duration_ms)
            .unwrap_or(0);
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

    let (tool_call_failure_rate, tool_call_failures_by_tool) =
        callable_tool_call_failure_stats(&tool_aggs);

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
        failure_incidents_truncated: incidents.truncated,
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
        tool_call_failure_rate,
        tool_call_failures_by_tool,
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

/// `parse_since`, with relative durations measured from `now`.
///
/// Absolute timestamps still go through `parse_since`, so their error text
/// is unchanged. Sharing `now` with the sparkline keeps a `30d` window on
/// one bucket count instead of drifting when the two clocks cross an hour.
fn summary_since(raw: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, OrbitError> {
    match parse_duration_seconds(raw) {
        Ok(seconds) => {
            let too_large = || {
                OrbitError::InvalidInput(format!(
                    "duration '{raw}' is too large to convert into a timestamp"
                ))
            };
            let seconds = i64::try_from(seconds).map_err(|_| too_large())?;
            let duration = Duration::try_seconds(seconds).ok_or_else(too_large)?;
            now.checked_sub_signed(duration).ok_or_else(too_large)
        }
        Err(_) => parse_since(raw),
    }
}

/// Inclusive UTC hours from the truncated `since` through `now`.
///
/// This is arithmetic only. A year-0001 cutoff becomes a large integer and
/// fails the maximum-bucket check; it does not allocate a row per hour. A
/// span that does not fit in `usize` saturates so it cannot wrap into a
/// small accepted window.
fn sparkline_bucket_count(since: DateTime<Utc>, now: DateTime<Utc>) -> usize {
    let start = truncate_to_hour(since.min(now));
    let end = truncate_to_hour(now);
    let hours = end.signed_duration_since(start).num_hours();
    usize::try_from(hours.saturating_add(1)).unwrap_or(usize::MAX)
}

/// Builds a contiguous hourly sparkline covering `[truncate_to_hour(since), now]`,
/// zero-filling hours not present in `buckets`. Always returns at least 24
/// buckets so the UI can render a stable baseline width even on a fresh
/// workspace.
///
/// The loop runs at most [`MAX_SUMMARY_SPARKLINE_BUCKETS`] times.
/// `audit_summary` rejects a wider window with 400 before calling this.
fn build_sparkline(
    since: DateTime<Utc>,
    now: DateTime<Utc>,
    buckets: &[(String, i64)],
) -> Vec<Value> {
    let mut by_bucket: BTreeMap<String, i64> = BTreeMap::new();
    for (ts, count) in buckets {
        by_bucket.insert(ts.clone(), *count);
    }
    let start = truncate_to_hour(since.min(now));
    let end = truncate_to_hour(now);
    let hours = sparkline_bucket_count(since, now).min(MAX_SUMMARY_SPARKLINE_BUCKETS);
    let mut out = Vec::with_capacity(hours.max(24));
    let mut cursor = start;
    for _ in 0..hours {
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
    for state in FAILED_RUN_STATES {
        total = total.saturating_add(runtime.count_job_runs(JobRunListParams {
            job_id: None,
            state: Some(state),
            terminal_only: false,
            since: Some(since),
            limit: None,
            ..Default::default()
        })?);
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

/// Raw callable-tool failure rate for the audit-summary pane.
///
/// Counts `command = tool`, `subcommand IN ('run', 'run-mcp')` rows on named, non-diagnostic
/// surfaces. The numerator is `status = failure` (expected negatives included);
/// the denominator is success + failure, with denied rows reported separately.
/// Unexpected counts use the same classifier as the incident card. Every tool
/// with at least one failure or denial is listed — unlike unexpected
/// `failure_rate_by_tool`, this is not
/// sample-size gated and is not truncated.
fn callable_tool_call_failure_stats(tool_aggs: &[AuditToolAggregate]) -> (Value, Vec<Value>) {
    let mut failed: i64 = 0;
    let mut total: i64 = 0;
    let mut unexpected: i64 = 0;
    let mut denied: i64 = 0;
    let mut by_tool = Vec::new();
    for tool in tool_aggs {
        if !is_named_tool(&tool.tool_name) || is_failure_only_diagnostic_surface(&tool.tool_name) {
            continue;
        }
        let tool_total = tool.mcp_total + tool.cli_total - tool.callable_denials;
        let tool_failed = tool.mcp_failures + tool.cli_failures;
        failed += tool_failed;
        total += tool_total;
        unexpected += tool.callable_unexpected_failures;
        denied += tool.callable_denials;
        if tool_failed > 0 || tool.callable_denials > 0 {
            let rate = if tool_total > 0 {
                tool_failed as f64 / tool_total as f64
            } else {
                0.0
            };
            by_tool.push(json!({
                "tool": tool.tool_name,
                "failed": tool_failed,
                "total": tool_total,
                "rate": rate,
                "unexpected": tool.callable_unexpected_failures,
                "denied": tool.callable_denials,
            }));
        }
    }
    by_tool.sort_by(|a, b| {
        b["failed"]
            .as_i64()
            .unwrap_or(0)
            .cmp(&a["failed"].as_i64().unwrap_or(0))
            .then_with(|| {
                a["tool"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(b["tool"].as_str().unwrap_or(""))
            })
    });
    let rate = if total > 0 {
        failed as f64 / total as f64
    } else {
        0.0
    };
    (
        json!({
            "failed": failed,
            "total": total,
            "rate": rate,
            "unexpected": unexpected,
            "denied": denied,
            "denominator": "successful + failed callable tool calls (run + run-mcp); denied excluded",
        }),
        by_tool,
    )
}
