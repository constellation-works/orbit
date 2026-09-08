//! Policy and tool denial aggregation across the v2 audit envelope and the
//! SQLite audit-events table.

use std::collections::BTreeMap;

use crate::state::Ws;
use axum::extract::Query;
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Utc};
use orbit_core::{AuditEventStatus, OrbitRuntime, V2AuditEventFilter};
use orbit_types::workflow::activity_job::{
    V2_DENIAL_EVENT_TYPES, V2_EVENT_TYPE_FS_CALL_DENIED, V2_EVENT_TYPE_STEP_DENIED,
};
use serde_json::{Value, json};

use super::{DEFAULT_SUMMARY_WINDOW, DenialsQuery, bad_request, map_runtime_error};
use crate::parse::parse_since;

const SQLITE_DENIAL_SCAN_LIMIT: usize = 1000;
pub(super) const SQLITE_FS_BOUNDARY_PROFILE: &str = "workspace-boundary";
const SQLITE_TOOL_DENIAL_PROFILE: &str = "tool";

/// Internal denial event extracted from the v2 envelope JSONL.
#[derive(Debug, Clone)]
pub(super) struct DenialRow {
    kind: &'static str,
    profile: String,
    target: String,
    job_run_id: Option<String>,
    execution_id: Option<String>,
    agent: String,
    timestamp: Option<DateTime<Utc>>,
    diagnostics: DenialDiagnostics,
}

#[derive(Debug, Clone)]
struct DenialDiagnostics {
    denial_kind: String,
    cause: String,
    actor: Option<String>,
    requested_task_ids: Vec<String>,
    requested_files: Vec<String>,
    conflicts: Vec<Value>,
}

impl Default for DenialDiagnostics {
    fn default() -> Self {
        Self {
            denial_kind: "unknown".to_string(),
            cause: "unknown".to_string(),
            actor: None,
            requested_task_ids: Vec::new(),
            requested_files: Vec::new(),
            conflicts: Vec::new(),
        }
    }
}

impl DenialRow {
    #[cfg(test)]
    pub(super) fn target(&self) -> &str {
        &self.target
    }
}

/// Reads SQLite v2 audit rows and returns FsCallDenied / ToolDenied rows
/// matching the supplied filters.
pub(super) fn scan_v2_loop_denials(
    runtime: &OrbitRuntime,
    since: Option<DateTime<Utc>>,
    profile_filter: Option<&str>,
    agent_filter: Option<&str>,
) -> Result<Vec<DenialRow>, orbit_core::OrbitError> {
    let mut rows = Vec::new();
    for event_type in v2_denial_event_types() {
        let events = OrbitRuntime::list_v2_audit_events(
            runtime,
            V2AuditEventFilter {
                workspace_id: String::new(),
                since,
                event_type: Some((*event_type).to_string()),
                limit: Some(SQLITE_DENIAL_SCAN_LIMIT),
                ..Default::default()
            },
        )?;
        for event in events {
            let value: Value = match serde_json::from_str(&event.payload_json) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let kind = v2_denial_kind(&event.event_type);
            let (profile, target) = match kind {
                "fs" => (
                    value
                        .get("profile")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    value
                        .get("path")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                ),
                _ if event.event_type == V2_EVENT_TYPE_STEP_DENIED => (
                    "tool".to_string(),
                    value
                        .get("step_id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                ),
                _ => (
                    "tool".to_string(),
                    value
                        .get("tool_name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                ),
            };
            if let Some(want) = profile_filter
                && !want.is_empty()
                && profile != want
            {
                continue;
            }
            if let Some(want) = agent_filter
                && !want.is_empty()
                && event.agent_identity != want
            {
                continue;
            }
            rows.push(DenialRow {
                kind,
                diagnostics: v2_denial_diagnostics(kind, &profile, &target),
                profile,
                target,
                job_run_id: Some(event.run_id).filter(|value| !value.is_empty()),
                execution_id: None,
                agent: event.agent_identity,
                timestamp: Some(event.ts),
            });
        }
    }
    Ok(rows)
}

pub(super) fn v2_denial_event_types() -> &'static [&'static str] {
    V2_DENIAL_EVENT_TYPES
}

fn v2_denial_kind(event_type: &str) -> &'static str {
    if event_type == V2_EVENT_TYPE_FS_CALL_DENIED {
        "fs"
    } else {
        "tool"
    }
}

pub(super) fn collect_denial_rows(
    runtime: &OrbitRuntime,
    since: Option<DateTime<Utc>>,
    profile_filter: Option<&str>,
    agent_filter: Option<&str>,
) -> Result<Vec<DenialRow>, orbit_core::OrbitError> {
    let mut rows = scan_v2_loop_denials(runtime, since, profile_filter, agent_filter)?;
    rows.extend(scan_sqlite_denials(
        runtime,
        since,
        profile_filter,
        agent_filter,
    )?);
    Ok(rows)
}

fn scan_sqlite_denials(
    runtime: &OrbitRuntime,
    since: Option<DateTime<Utc>>,
    profile_filter: Option<&str>,
    agent_filter: Option<&str>,
) -> Result<Vec<DenialRow>, orbit_core::OrbitError> {
    let events = OrbitRuntime::list_audit_events(
        runtime,
        since,
        None,
        Some(AuditEventStatus::Denied),
        agent_filter.map(ToOwned::to_owned),
        SQLITE_DENIAL_SCAN_LIMIT,
    )?;

    let rows = events
        .into_iter()
        .map(|event| sqlite_denial_row(&event))
        .filter(|row| {
            profile_filter
                .map(|want| want.is_empty() || row.profile == want)
                .unwrap_or(true)
        })
        .collect();
    Ok(rows)
}

fn sqlite_denial_row(event: &orbit_core::AuditEvent) -> DenialRow {
    let kind = sqlite_denial_kind(event);
    let arguments_json = parse_arguments_json(event.arguments_json.as_deref());
    let diagnostics = sqlite_denial_diagnostics(event, kind, arguments_json.as_ref());
    DenialRow {
        kind,
        profile: sqlite_denial_profile(event, kind, arguments_json.as_ref()),
        target: sqlite_denial_target(event, kind, arguments_json.as_ref()),
        job_run_id: event.job_run_id.clone().filter(|value| !value.is_empty()),
        execution_id: Some(event.execution_id.clone()).filter(|value| !value.is_empty()),
        agent: event.role.clone(),
        timestamp: Some(event.timestamp),
        diagnostics,
    }
}

fn sqlite_denial_kind(event: &orbit_core::AuditEvent) -> &'static str {
    let tool_name = event.tool_name.as_deref().unwrap_or("");
    if tool_name.starts_with("fs.") {
        "fs"
    } else {
        "tool"
    }
}

fn sqlite_denial_profile(
    event: &orbit_core::AuditEvent,
    kind: &str,
    arguments_json: Option<&Value>,
) -> String {
    if kind != "fs" {
        return SQLITE_TOOL_DENIAL_PROFILE.to_string();
    }
    if let Some(profile) = arguments_json_profile(arguments_json) {
        return profile;
    }
    if let Some(profile) = extract_fs_profile_from_policy_message(event.error_message.as_deref()) {
        return profile;
    }
    SQLITE_FS_BOUNDARY_PROFILE.to_string()
}

fn sqlite_denial_target(
    event: &orbit_core::AuditEvent,
    kind: &str,
    arguments_json: Option<&Value>,
) -> String {
    if kind == "fs"
        && let Some(path) = extract_fs_path_from_policy_message(event.error_message.as_deref())
    {
        return path;
    }
    if is_task_lock_reserve_denial(event) {
        let requested_files = string_array_field(arguments_json, "files");
        if let Some(file) = requested_files.first() {
            return file.clone();
        }
        let task_ids = string_array_field(arguments_json, "task_ids");
        if !task_ids.is_empty() {
            return task_ids.join(", ");
        }
    }
    event
        .target_id
        .clone()
        .or_else(|| event.tool_name.clone())
        .or_else(|| event.subcommand.clone())
        .unwrap_or_else(|| event.command.clone())
}

fn parse_arguments_json(raw: Option<&str>) -> Option<Value> {
    serde_json::from_str(raw?).ok()
}

fn arguments_json_profile(value: Option<&Value>) -> Option<String> {
    const KEYS: &[&str] = &["fsProfile", "fs_profile", "profile"];
    let obj = value?.as_object()?;
    for key in KEYS {
        if let Some(Value::String(found)) = obj.get(*key)
            && !found.is_empty()
        {
            return Some(found.clone());
        }
    }
    None
}

fn v2_denial_diagnostics(kind: &str, profile: &str, target: &str) -> DenialDiagnostics {
    match kind {
        "fs" => DenialDiagnostics {
            denial_kind: "fs_policy".to_string(),
            cause: if profile.is_empty() {
                "fs_denied".to_string()
            } else {
                profile.to_string()
            },
            requested_files: if target.is_empty() {
                Vec::new()
            } else {
                vec![target.to_string()]
            },
            ..DenialDiagnostics::default()
        },
        _ => DenialDiagnostics {
            denial_kind: "tool_policy".to_string(),
            cause: if target.is_empty() {
                "tool_denied".to_string()
            } else {
                target.to_string()
            },
            ..DenialDiagnostics::default()
        },
    }
}

fn sqlite_denial_diagnostics(
    event: &orbit_core::AuditEvent,
    kind: &str,
    arguments_json: Option<&Value>,
) -> DenialDiagnostics {
    if is_task_lock_reserve_denial(event) {
        let conflicts = value_array_field(arguments_json, "conflicts");
        return DenialDiagnostics {
            denial_kind: "task_lock_reserve".to_string(),
            cause: if conflicts.is_empty() {
                "task_lock_denied".to_string()
            } else {
                "task_lock_conflict".to_string()
            },
            actor: string_field(arguments_json, "actor"),
            requested_task_ids: string_array_field(arguments_json, "task_ids"),
            requested_files: string_array_field(arguments_json, "files"),
            conflicts,
        };
    }

    if kind == "fs" {
        let profile = sqlite_denial_profile(event, kind, arguments_json);
        return DenialDiagnostics {
            denial_kind: "fs_policy".to_string(),
            cause: profile,
            requested_files: extract_fs_path_from_policy_message(event.error_message.as_deref())
                .into_iter()
                .collect(),
            ..DenialDiagnostics::default()
        };
    }

    DenialDiagnostics {
        denial_kind: "tool_policy".to_string(),
        cause: event
            .tool_name
            .clone()
            .or_else(|| event.subcommand.clone())
            .unwrap_or_else(|| "tool_denied".to_string()),
        ..DenialDiagnostics::default()
    }
}

fn is_task_lock_reserve_denial(event: &orbit_core::AuditEvent) -> bool {
    event.tool_name.as_deref() == Some("orbit.task.locks.reserve")
        || event.command == "task.locks.reserve.denied"
}

fn string_field(value: Option<&Value>, key: &str) -> Option<String> {
    value?
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

fn string_array_field(value: Option<&Value>, key: &str) -> Vec<String> {
    value
        .and_then(|value| value.get(key))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn value_array_field(value: Option<&Value>, key: &str) -> Vec<Value> {
    value
        .and_then(|value| value.get(key))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn extract_fs_profile_from_policy_message(message: Option<&str>) -> Option<String> {
    extract_between(message?, "under fsProfile `", "`")
}

fn extract_fs_path_from_policy_message(message: Option<&str>) -> Option<String> {
    let message = message?;
    if let Some(path) = extract_denied_for_path(message) {
        return Some(path);
    }
    extract_after_prefix(message, "path is outside workspace: ")
}

fn extract_denied_for_path(message: &str) -> Option<String> {
    let marker = " denied for `";
    let marker_idx = message.find(marker)?;
    let prefix = &message[..marker_idx];
    // Historical policy messages used `fs.<op> denied for \`...\``. Match any
    // `fs.*` prefix so stored traces from retired builtins still parse.
    let (head, op) = prefix.rsplit_once('.')?;
    if !head.ends_with("fs") || op.is_empty() {
        return None;
    }
    let rest = &message[marker_idx + marker.len()..];
    let end = rest.find('`')?;
    let path = rest[..end].trim();
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

fn extract_between(message: &str, start: &str, end: &str) -> Option<String> {
    let start_idx = message.find(start)? + start.len();
    let rest = &message[start_idx..];
    let end_idx = rest.find(end)?;
    let value = rest[..end_idx].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn extract_after_prefix(message: &str, prefix: &str) -> Option<String> {
    let start_idx = message.find(prefix)? + prefix.len();
    let value = message[start_idx..]
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .trim_end_matches('.');
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// Top-N tool denials for the audit-summary side panel. Targets `kind == "tool"`
/// rows (so `target` is a tool name); fs rows are excluded because their `target`
/// is a path and would clutter the per-tool list.
pub(super) fn denials_by_tool_summary(rows: &[DenialRow], limit: usize) -> Value {
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    for row in rows {
        if row.kind != "tool" {
            continue;
        }
        if row.target.is_empty() {
            continue;
        }
        *counts.entry(row.target.clone()).or_insert(0) += 1;
    }
    let mut out: Vec<_> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Value::Array(
        out.into_iter()
            .take(limit)
            .map(|(tool, count)| json!({"tool": tool, "count": count}))
            .collect(),
    )
}

/// Top-N denial causes (fs profile, tool name, lock conflict, etc.) across all
/// denial kinds for the audit-summary side panel.
pub(super) fn denials_by_reason_summary(rows: &[DenialRow], limit: usize) -> Value {
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    for row in rows {
        let cause = row.diagnostics.cause.clone();
        if cause.is_empty() {
            continue;
        }
        *counts.entry(cause).or_insert(0) += 1;
    }
    let mut out: Vec<_> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Value::Array(
        out.into_iter()
            .take(limit)
            .map(|(reason, count)| json!({"reason": reason, "count": count}))
            .collect(),
    )
}

pub(super) async fn list_denials(Ws(runtime): Ws, Query(q): Query<DenialsQuery>) -> Response {
    let raw_since = q.since.as_deref().unwrap_or(DEFAULT_SUMMARY_WINDOW);
    let since = match parse_since(raw_since) {
        Ok(ts) => Some(ts),
        Err(e) => return map_runtime_error(e),
    };
    let kind = q
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase);
    if let Some(ref k) = kind
        && k != "fs"
        && k != "tool"
    {
        return bad_request(format!("kind must be 'fs', 'tool', or omitted; got '{k}'"));
    }
    let profile_filter = q
        .profile
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let agent_filter = q
        .agent
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    // One SQLite scan per denial event type plus a v2 loop scan: blocking
    // pool, not the worker serving the request (see `blocking`).
    let rows = match super::blocking("denials listing", move || {
        collect_denial_rows(
            &runtime,
            since,
            profile_filter.as_deref(),
            agent_filter.as_deref(),
        )
    })
    .await
    {
        Ok(rows) => rows,
        Err(response) => return *response,
    };

    Json(denials_payload(&rows, kind.as_deref(), since)).into_response()
}

// Widened to pub(super) for api/tests/ access after test layout migration (ORB-00224).
pub(super) fn denials_payload(
    rows: &[DenialRow],
    kind: Option<&str>,
    since: Option<DateTime<Utc>>,
) -> Value {
    let filtered = filter_denial_rows(rows, kind);

    let by_profile = aggregate_by(&filtered, |r| r.profile.clone());
    let by_target = aggregate_by(&filtered, |r| r.target.clone());
    let by_run = aggregate_by_optional(&filtered, |r| r.job_run_id.clone());
    let by_execution = aggregate_by_optional(&filtered, |r| {
        if r.job_run_id.is_none() {
            r.execution_id.clone()
        } else {
            None
        }
    });
    let by_agent = aggregate_by(&filtered, |r| r.agent.clone());
    let top_causes = top_causes_to_value(&filtered);
    let recent_denials = recent_denials_to_value(&filtered, 12);

    json!({
        "by_profile": rows_to_value(&by_profile, "name"),
        "by_target": rows_to_value(&by_target, "name"),
        "by_run": rows_to_value(&by_run, "run_id"),
        "by_execution": rows_to_value(&by_execution, "execution_id"),
        "by_agent": rows_to_value(&by_agent, "agent"),
        "top_causes": top_causes,
        "recent_denials": recent_denials,
        "total": filtered.len(),
        "kind": kind,
        "since": since.map(|s| s.to_rfc3339()),
    })
}

fn filter_denial_rows<'a>(rows: &'a [DenialRow], kind: Option<&str>) -> Vec<&'a DenialRow> {
    rows.iter()
        .filter(|r| match kind {
            None => true,
            Some(k) => r.kind == k,
        })
        .collect()
}

fn aggregate_by<F>(rows: &[&DenialRow], key: F) -> Vec<(String, i64)>
where
    F: Fn(&DenialRow) -> String,
{
    aggregate_by_optional(rows, |row| Some(key(row)))
}

fn aggregate_by_optional<F>(rows: &[&DenialRow], key: F) -> Vec<(String, i64)>
where
    F: Fn(&DenialRow) -> Option<String>,
{
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    for row in rows {
        let Some(k) = key(row) else {
            continue;
        };
        if k.is_empty() {
            continue;
        }
        *counts.entry(k).or_insert(0) += 1;
    }
    let mut out: Vec<_> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

fn rows_to_value(rows: &[(String, i64)], key_label: &str) -> Value {
    Value::Array(
        rows.iter()
            .map(|(name, count)| json!({ key_label: name, "count": count }))
            .collect(),
    )
}

#[derive(Debug, Default)]
struct CauseAggregate {
    count: i64,
    latest: Option<DateTime<Utc>>,
    targets: BTreeMap<String, i64>,
}

fn top_causes_to_value(rows: &[&DenialRow]) -> Value {
    let mut by_cause: BTreeMap<String, CauseAggregate> = BTreeMap::new();
    for row in rows {
        let cause = row.diagnostics.cause.clone();
        if cause.is_empty() {
            continue;
        }
        let entry = by_cause.entry(cause).or_default();
        entry.count += 1;
        if row.timestamp > entry.latest {
            entry.latest = row.timestamp;
        }
        if !row.target.is_empty() {
            *entry.targets.entry(row.target.clone()).or_insert(0) += 1;
        }
    }

    let mut out: Vec<_> = by_cause.into_iter().collect();
    out.sort_by(|(left_cause, left), (right_cause, right)| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| right.latest.cmp(&left.latest))
            .then_with(|| left_cause.cmp(right_cause))
    });
    Value::Array(
        out.into_iter()
            .take(8)
            .map(|(cause, aggregate)| {
                let target = aggregate
                    .targets
                    .into_iter()
                    .max_by(|left, right| left.1.cmp(&right.1).then_with(|| right.0.cmp(&left.0)))
                    .map(|(target, _count)| target);
                json!({
                    "cause": cause,
                    "target": target,
                    "count": aggregate.count,
                    "latest_ts": aggregate.latest.map(|ts| ts.to_rfc3339()),
                })
            })
            .collect(),
    )
}

fn recent_denials_to_value(rows: &[&DenialRow], limit: usize) -> Value {
    let mut rows = rows.to_vec();
    rows.sort_by(|left, right| {
        right
            .timestamp
            .cmp(&left.timestamp)
            .then_with(|| left.target.cmp(&right.target))
    });
    Value::Array(
        rows.into_iter()
            .take(limit)
            .map(denial_row_to_value)
            .collect(),
    )
}

fn denial_row_to_value(row: &DenialRow) -> Value {
    let (identity_type, identity_id) = if let Some(job_run_id) = row.job_run_id.as_deref() {
        ("job_run", Some(job_run_id))
    } else if let Some(execution_id) = row.execution_id.as_deref() {
        ("audit_execution", Some(execution_id))
    } else {
        ("none", None)
    };

    json!({
        "kind": row.kind,
        "profile": row.profile.clone(),
        "target": row.target.clone(),
        "run_id": row.job_run_id.clone(),
        "job_run_id": row.job_run_id.clone(),
        "execution_id": row.execution_id.clone(),
        "identity_type": identity_type,
        "identity_id": identity_id,
        "agent": row.agent.clone(),
        "timestamp": row.timestamp.map(|ts| ts.to_rfc3339()),
        "denial_kind": row.diagnostics.denial_kind.clone(),
        "cause": row.diagnostics.cause.clone(),
        "actor": row.diagnostics.actor.clone(),
        "requested_task_ids": row.diagnostics.requested_task_ids.clone(),
        "requested_files": row.diagnostics.requested_files.clone(),
        "conflicts": row.diagnostics.conflicts.clone(),
    })
}
