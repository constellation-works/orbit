//! `/metrics/*` parity endpoints for the retired CLI metrics views.

use crate::state::Ws;
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Utc};
use orbit_core::application::job::JobRunListParams;
use orbit_core::metrics::aggregate as aggregate_knowledge_stats;
use orbit_core::{InvocationInsertParams, InvocationQuery};
use serde::Deserialize;

use super::{LimitQuery, blocking, map_runtime_error, non_empty_string};

const INVOCATIONS_DEFAULT_LIMIT: usize = 20;

#[derive(Debug, Default, Deserialize)]
pub(super) struct MetricsInvocationsQuery {
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    until: Option<String>,
    #[serde(default)]
    job_run_id: Option<String>,
    #[serde(default)]
    activity_id: Option<String>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct OrchestratorMetricsQuery {
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    until: Option<String>,
}

pub(super) async fn knowledge_metrics(Ws(runtime): Ws, Query(q): Query<LimitQuery>) -> Response {
    match blocking("knowledge metrics", move || {
        runtime.list_job_runs(JobRunListParams {
            limit: q.limit,
            ..Default::default()
        })
    })
    .await
    {
        Ok(runs) => Json(aggregate_knowledge_stats(&runs)).into_response(),
        Err(response) => *response,
    }
}

pub(super) async fn activity_metrics(Ws(runtime): Ws) -> Response {
    match blocking("activity metrics", move || {
        runtime.activity_invocation_metrics()
    })
    .await
    {
        Ok(rows) => Json(rows).into_response(),
        Err(response) => *response,
    }
}

pub(super) async fn tool_metrics(Ws(runtime): Ws) -> Response {
    match blocking("tool metrics", move || runtime.tool_invocation_metrics()).await {
        Ok(rows) => Json(rows).into_response(),
        Err(response) => *response,
    }
}

pub(super) async fn task_metrics(Ws(runtime): Ws, Path(id): Path<String>) -> Response {
    match blocking("task metrics", move || runtime.task_invocation_metrics(&id)).await {
        Ok(row) => Json(row).into_response(),
        Err(response) => *response,
    }
}

/// Returns managed-execution invocation accounting grouped by the explicit
/// orchestrator ownership on canonical tasks. Direct provider sessions that
/// do not write managed invocation rows are outside this metric.
pub(super) async fn orchestrator_metrics(
    Ws(runtime): Ws,
    Query(q): Query<OrchestratorMetricsQuery>,
) -> Response {
    let since = match parse_rfc3339_opt(q.since, "since") {
        Ok(value) => value,
        Err(error) => return map_runtime_error(error),
    };
    let until = match parse_rfc3339_opt(q.until, "until") {
        Ok(value) => value,
        Err(error) => return map_runtime_error(error),
    };
    match blocking("orchestrator metrics", move || {
        runtime.orchestrator_invocation_metrics(since, until)
    })
    .await
    {
        Ok(metrics) => Json(metrics).into_response(),
        Err(response) => *response,
    }
}

pub(super) async fn invocation_metrics(
    Ws(runtime): Ws,
    Query(q): Query<MetricsInvocationsQuery>,
) -> Response {
    let query = match q.into_invocation_query() {
        Ok(query) => query,
        Err(e) => return map_runtime_error(e),
    };
    match blocking("invocation metrics", move || {
        runtime.invocation_records(query)
    })
    .await
    {
        Ok(rows) => Json(rows).into_response(),
        Err(response) => *response,
    }
}

pub(super) async fn ingest_invocation(
    Ws(runtime): Ws,
    Json(params): Json<InvocationInsertParams>,
) -> Response {
    match blocking("ingest invocation", move || {
        runtime.insert_invocation_trace_record(&params)
    })
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(response) => *response,
    }
}

impl MetricsInvocationsQuery {
    fn into_invocation_query(self) -> Result<InvocationQuery, orbit_core::OrbitError> {
        Ok(InvocationQuery {
            since: parse_rfc3339_opt(self.since, "since")?,
            until: parse_rfc3339_opt(self.until, "until")?,
            job_run_id: optional_query_string(self.job_run_id),
            activity_id: optional_query_string(self.activity_id),
            task_id: optional_query_string(self.task_id),
            agent: optional_query_string(self.agent),
            model: optional_query_string(self.model),
            tool_name: optional_query_string(self.tool_name),
            limit: self.limit.unwrap_or(INVOCATIONS_DEFAULT_LIMIT),
        })
    }
}

fn parse_rfc3339_opt(
    value: Option<String>,
    field_name: &str,
) -> Result<Option<DateTime<Utc>>, orbit_core::OrbitError> {
    match optional_query_string(value) {
        Some(raw) => DateTime::parse_from_rfc3339(&raw)
            .map(|dt| Some(dt.with_timezone(&Utc)))
            .map_err(|error| {
                orbit_core::OrbitError::InvalidInput(format!("invalid {field_name}: {error}"))
            }),
        None => Ok(None),
    }
}

fn optional_query_string(value: Option<String>) -> Option<String> {
    value.as_deref().and_then(non_empty_string)
}
