//! Metrics ranges and invocation row projection.

use axum::extract::Query;
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Utc};
use orbit_cmd::DiagnosticsCommands;
use orbit_core::{InvocationQuery, InvocationRecord};
use serde_json::{Value, json};

use super::super::{
    DiagnosticsQuery, HISTORY_DEFAULT_LIMIT, bounded_limit, current_year_month_utc,
    map_runtime_error, month_bounds_utc,
};
use crate::state::Ws;

pub(in crate::api) async fn list_diagnostics_metrics(
    Ws(runtime): Ws,
    Query(q): Query<DiagnosticsQuery>,
) -> Response {
    let range = match DiagnosticsRange::from_query(&q, true) {
        Ok(range) => range,
        Err(error) => return map_runtime_error(error),
    };
    let limit = bounded_limit(q.limit, HISTORY_DEFAULT_LIMIT);
    match super::super::blocking("diagnostics metrics", move || {
        if limit == 0 {
            return Ok(json!([]));
        }
        let mut entries = Vec::new();
        for month in runtime.list_metrics_months()? {
            let (start, end) = month_bounds_utc(&month)?;
            if range.since.is_some_and(|since| end < since) || start > range.until {
                continue;
            }
            entries.extend(
                runtime
                    .read_metrics_entries(&month)?
                    .into_iter()
                    .filter(|entry| range.contains(entry.ts)),
            );
        }
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.ts));
        entries.truncate(limit);
        if entries.is_empty() {
            let records = runtime.invocation_records(InvocationQuery {
                since: range.since,
                until: Some(range.until),
                limit,
                ..InvocationQuery::default()
            })?;
            Ok(Value::Array(diagnostics_metrics_values(records)))
        } else {
            serde_json::to_value(&entries).map_err(|e| orbit_core::OrbitError::Store(e.to_string()))
        }
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

/// Legacy month requests remain supported; dashboard requests supply `since`.
pub(super) struct DiagnosticsRange {
    pub(super) key: String,
    pub(super) since: Option<DateTime<Utc>>,
    pub(super) until: DateTime<Utc>,
}

impl DiagnosticsRange {
    pub(super) fn from_query(
        q: &DiagnosticsQuery,
        default_month: bool,
    ) -> Result<Self, orbit_core::OrbitError> {
        if q.since.is_some() && q.month.is_some() {
            return Err(orbit_core::OrbitError::InvalidInput(
                "supply either since or month, not both".to_string(),
            ));
        }
        if let Some(raw) = q.since.as_deref() {
            let raw = raw.trim();
            return Ok(Self {
                key: raw.to_string(),
                since: if raw == "all" {
                    None
                } else {
                    Some(crate::parse::parse_since(raw)?)
                },
                until: Utc::now(),
            });
        }
        let month = q
            .month
            .clone()
            .or_else(|| default_month.then(current_year_month_utc));
        if let Some(month) = month {
            let (since, until) = month_bounds_utc(&month)?;
            Ok(Self {
                key: month,
                since: Some(since),
                until,
            })
        } else {
            Ok(Self {
                key: "all".to_string(),
                since: None,
                until: Utc::now(),
            })
        }
    }

    pub(super) fn contains(&self, ts: DateTime<Utc>) -> bool {
        self.since.is_none_or(|since| ts >= since) && ts <= self.until
    }
}

pub(super) fn diagnostics_metrics_values(records: Vec<InvocationRecord>) -> Vec<Value> {
    records
        .into_iter()
        .map(|record| {
            json!({
                "ts": record.ts.to_rfc3339(),
                "job_run": record.job_run_id,
                "step": record.activity_id,
                "task_id": record.task_ids.first().cloned(),
                "actor_identity": actor_label(&record.agent, record.model.as_deref()),
                "tool_invocations": record.tool_call_count,
                "token_usage": record.total_tokens,
                "step_duration_ms": record.duration_ms,
                "retry_count": 0,
            })
        })
        .collect()
}

pub(super) fn actor_label(agent: &str, model: Option<&str>) -> String {
    match model.filter(|model| !model.is_empty()) {
        Some(model) if !agent.is_empty() => format!("{agent} / {model}"),
        Some(model) => model.to_string(),
        None => agent.to_string(),
    }
}
