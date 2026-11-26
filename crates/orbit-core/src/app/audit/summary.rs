use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contract::{
    AuditActorAggregate, AuditAttributionAggregate, AuditToolAggregate, AuditToolModelAggregate,
    FailureIncidentQuery, FailureIncidentReport,
};

use crate::application::audit_event::AuditPolicyDenialStats;
use crate::application::job::JobRunListParams;
use crate::{Core, JobRunState};

const FAILED_RUN_STATES: [JobRunState; 3] = [
    JobRunState::Failed,
    JobRunState::Timeout,
    JobRunState::Interrupted,
];

/// Top-N slowest named tool summary entry with its p95 duration.
pub struct AuditToolDuration {
    /// Tool name.
    pub tool: String,
    /// Total invocations in the window.
    pub count: i64,
    /// Mean duration in milliseconds.
    pub avg: f64,
    /// 95th-percentile duration in milliseconds.
    pub p95: i64,
}

/// Read model backing `GET /api/audit/summary`.
pub struct AuditSummary {
    /// Uncapped SQL total of audit events in the window.
    pub total: i64,
    /// Canonical policy-denial totals.
    pub policy: AuditPolicyDenialStats,
    /// Grouped failure incidents over the window.
    pub incidents: FailureIncidentReport,
    /// Failed, timed-out or interrupted job runs in the window.
    pub failed_runs: i64,
    /// Active running job runs exceeding the window's p95 finished duration.
    pub active_long_runs: i64,
    /// Non-empty hourly event-count buckets.
    pub buckets: Vec<(String, i64)>,
    /// Per-tool aggregates over the window.
    pub tool_aggs: Vec<AuditToolAggregate>,
    /// Per-tool and session-model aggregates over the window.
    pub tool_model_aggs: Vec<AuditToolModelAggregate>,
    /// Per-canonical-actor aggregates over the window.
    pub actor_aggs: Vec<AuditActorAggregate>,
    /// Tool-call counts split by identity attribution.
    pub attribution_aggs: Vec<AuditAttributionAggregate>,
    /// Top 8 slowest named tools with their p95 durations.
    pub duration_by_tool: Vec<AuditToolDuration>,
}

pub(super) fn read(
    core: &Core,
    since: DateTime<Utc>,
    incident_scan_limit: usize,
) -> Result<AuditSummary, OrbitError> {
    let runtime = core.runtime();
    let stats = runtime.audit_event_stats(Some(since), None)?;
    let policy = runtime.audit_policy_denial_stats(Some(&since))?;
    let incidents = runtime.audit_failure_incidents(&FailureIncidentQuery {
        since: Some(since),
        max_events: incident_scan_limit,
        ..Default::default()
    })?;
    let failed_runs = count_failed_runs(core, since)?;
    let active_long_runs = count_active_long_runs(core, since)?;
    let buckets = runtime.audit_event_hourly_buckets(&since)?;
    let tool_aggs = runtime.audit_event_aggregates_by_tool(&since)?;
    let tool_model_aggs = runtime.audit_event_aggregates_by_tool_model(&since)?;
    let actor_aggs = runtime.audit_event_aggregates_by_actor(&since)?;
    let attribution_aggs = runtime.audit_tool_call_counts_by_attribution(Some(&since))?;

    let mut by_avg: Vec<&AuditToolAggregate> = tool_aggs
        .iter()
        .filter(|tool| is_named_tool(&tool.tool_name))
        .collect();
    by_avg.sort_by(|left, right| {
        right
            .avg_duration_ms
            .partial_cmp(&left.avg_duration_ms)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut duration_by_tool = Vec::with_capacity(8);
    for tool in by_avg.iter().take(8) {
        let p95 = runtime
            .audit_event_stats(Some(since), Some(tool.tool_name.clone()))
            .map(|stats| stats.p95_duration_ms)
            .unwrap_or(0);
        duration_by_tool.push(AuditToolDuration {
            tool: tool.tool_name.clone(),
            count: tool.total,
            avg: tool.avg_duration_ms,
            p95,
        });
    }

    Ok(AuditSummary {
        total: stats.total,
        policy,
        incidents,
        failed_runs,
        active_long_runs,
        buckets,
        tool_aggs,
        tool_model_aggs,
        actor_aggs,
        attribution_aggs,
        duration_by_tool,
    })
}

fn count_failed_runs(core: &Core, since: DateTime<Utc>) -> Result<i64, OrbitError> {
    let runtime = core.runtime();
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

fn count_active_long_runs(core: &Core, since: DateTime<Utc>) -> Result<i64, OrbitError> {
    let runtime = core.runtime();
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
        .map(|duration| i64::try_from(duration).unwrap_or(i64::MAX))
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
    for run in running {
        let started = run.started_at.unwrap_or(run.created_at);
        let elapsed = now.signed_duration_since(started).num_milliseconds().max(0);
        if elapsed > p95_ms {
            count += 1;
        }
    }
    Ok(count)
}

fn is_named_tool(name: &str) -> bool {
    let trimmed = name.trim();
    !trimmed.is_empty() && trimmed != "unknown"
}
