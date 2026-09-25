//! Summary generation: per-agent rollups, window filtering and the recent
//! window block.

use super::files::{read_model_scoreboard, read_token_agents};
use super::overlay::{
    family_key, overlay_audit_tool_calls, overlay_audit_tool_calls_by_surface,
    overlay_friction_reported, overlay_nested_metric, seed_known_family_agents,
};
use super::types::{CURRENT_SCHEMA_VERSION, RECENT_WINDOW_DAYS};
use super::{
    AgentSummary, RecentSummary, ScoreboardInputs, ScoreboardSummary, TopToolCall,
    WorkflowRunCount, select_notable_completions, snapshot_coverage,
};
use crate::AuditToolCallCountsByRole;
use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_types::identity::{normalize_attribution_label, normalize_optional_attribution_label};
use orbit_types::task::{Task, TaskStatus};
use orbit_types::workflow::{JobRun, JobRunState};
use std::collections::BTreeMap;
use std::path::Path;

pub fn generate_summary(
    scoreboard_dir: &Path,
    tasks: &[Task],
) -> Result<ScoreboardSummary, OrbitError> {
    generate_summary_with_inputs(scoreboard_dir, tasks, &ScoreboardInputs::default())
}

pub fn generate_summary_with_audit_tool_calls(
    scoreboard_dir: &Path,
    tasks: &[Task],
    audit_tool_calls: &[AuditToolCallCountsByRole],
) -> Result<ScoreboardSummary, OrbitError> {
    generate_summary_with_inputs(
        scoreboard_dir,
        tasks,
        &ScoreboardInputs {
            audit_tool_calls,
            ..ScoreboardInputs::default()
        },
    )
}

pub fn generate_summary_with_inputs(
    scoreboard_dir: &Path,
    tasks: &[Task],
    inputs: &ScoreboardInputs<'_>,
) -> Result<ScoreboardSummary, OrbitError> {
    let audit_tool_calls = inputs.audit_tool_calls;
    let mut agents: BTreeMap<String, AgentSummary> = BTreeMap::new();
    seed_known_family_agents(&mut agents);

    // Window cutoff. `None` (i.e. `window == All`) preserves the legacy
    // lifetime behavior; `Some(since)` triggers per-source filtering and
    // skips snapshot reads (which lack per-event timestamps).
    let now_for_window = inputs.now.unwrap_or_else(Utc::now);
    let since: Option<DateTime<Utc>> = inputs.window.duration().map(|d| now_for_window - d);
    let windowed = since.is_some();

    // Snapshot reads (pr.json, task_review.json, tokens.json) have no per-event timestamp, so they only run
    // for the lifetime (`All`) window. Under a windowed view they zero
    // out — the frontend renders 0 as `—` via emptyScoreboardNode().
    // TODO(phase-3+): timestamped snapshot logs would unblock real
    // windowing of these columns.
    if !windowed {
        let pr = read_model_scoreboard(scoreboard_dir)?;
        overlay_nested_metric(&mut agents, &pr, "pr-review-comments", |summary, value| {
            summary.pr.review_comments = summary.pr.review_comments.saturating_add(value);
        });
        overlay_nested_metric(
            &mut agents,
            &pr,
            "pr-count-without-revision",
            |summary, value| {
                summary.pr.merged_clean = summary.pr.merged_clean.saturating_add(value);
            },
        );
        overlay_nested_metric(
            &mut agents,
            &pr,
            "pr-count-with-revision",
            |summary, value| {
                summary.pr.merged_with_revision =
                    summary.pr.merged_with_revision.saturating_add(value);
            },
        );

        for token_row in read_token_agents(scoreboard_dir)? {
            let Some(model) = token_row
                .model
                .as_deref()
                .map(family_key)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            let summary = agents.entry(model).or_default();
            summary.tokens.total = summary.tokens.total.saturating_add(token_row.total_tokens);
            summary.tokens.output = summary
                .tokens
                .output
                .saturating_add(token_row.total_output_tokens);
            summary.tool_calls = summary
                .tool_calls
                .saturating_add(token_row.total_tool_calls);
        }
    }

    overlay_audit_tool_calls(&mut agents, audit_tool_calls);
    overlay_audit_tool_calls_by_surface(&mut agents, inputs.audit_tool_calls_by_surface);

    overlay_friction_reported(&mut agents, inputs.friction_reported);

    for task in tasks {
        if matches!(task.status, TaskStatus::Done | TaskStatus::Archived)
            && in_window(task_done_at(task), since)
            && let Some(model) = normalize_optional_attribution_label(
                task.implemented_by.as_deref(),
                task.implemented_by.as_deref(),
            )
        {
            let summary = agents.entry(family_key(&model)).or_default();
            summary.tasks_completed = summary.tasks_completed.saturating_add(1);
        }

        // Created/Planned count *all* statuses — see [T20260508-16]: rejected
        // tasks still represent real work the agent produced.
        // Windowed views filter by `created_at` so an old task created
        // outside the window doesn't get re-counted today.
        if in_window(Some(task.created_at), since) {
            if let Some(label) = task
                .created_by
                .as_deref()
                .map(|raw| normalize_attribution_label(raw, None))
                .filter(|value| !value.is_empty())
            {
                let summary = agents.entry(family_key(&label)).or_default();
                summary.tasks_created = summary.tasks_created.saturating_add(1);
            }
            if let Some(label) = task
                .planned_by
                .as_deref()
                .map(|raw| normalize_attribution_label(raw, None))
                .filter(|value| !value.is_empty())
            {
                let summary = agents.entry(family_key(&label)).or_default();
                summary.tasks_planned = summary.tasks_planned.saturating_add(1);
            }
        }
    }

    let workflows_run = aggregate_workflows_run(inputs.job_runs, since);
    let top_tools: Vec<TopToolCall> = inputs
        .top_tool_calls
        .iter()
        .map(|row| TopToolCall {
            role: row.role.clone(),
            tool_name: row.tool_name.clone(),
            count: row.total,
        })
        .collect();
    // recent_7d intentionally uses the full (unfiltered) tasks slice —
    // its 7d boundary is a fixed "is this still being used" signal,
    // independent of the user-selected `window`.
    let recent_7d = inputs
        .now
        .map(|now| build_recent_summary(now, tasks, inputs));
    Ok(ScoreboardSummary {
        schema_version: CURRENT_SCHEMA_VERSION,
        generated_at: Utc::now().to_rfc3339(),
        agents,
        workflows_run,
        top_tools,
        recent_7d,
        orchestration: inputs.orchestration.clone(),
        window: inputs.window.as_str().to_string(),
        window_since: since.map(|t| t.to_rfc3339()),
        notable_completions: select_notable_completions(tasks, since),
        coverage: snapshot_coverage(windowed),
    })
}

/// `true` when `timestamp` is at or after `since`. `since == None` means
/// the lifetime window — everything is in-window.
fn in_window(timestamp: Option<DateTime<Utc>>, since: Option<DateTime<Utc>>) -> bool {
    match (timestamp, since) {
        (_, None) => true,
        (Some(ts), Some(cut)) => ts >= cut,
        (None, Some(_)) => false,
    }
}

fn aggregate_workflows_run(runs: &[JobRun], since: Option<DateTime<Utc>>) -> Vec<WorkflowRunCount> {
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    for run in runs {
        if run.state == JobRunState::Success && in_window(Some(run_completed_at(run)), since) {
            *counts.entry(run.job_id.to_string()).or_insert(0) += 1;
        }
    }
    let mut rows: Vec<WorkflowRunCount> = counts
        .into_iter()
        .map(|(job_id, count)| WorkflowRunCount { job_id, count })
        .collect();
    // Highest run-count first; tie-break by job_id ASC for stable output.
    rows.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.job_id.cmp(&b.job_id)));
    rows
}

fn build_recent_summary(
    now: DateTime<Utc>,
    tasks: &[Task],
    inputs: &ScoreboardInputs<'_>,
) -> RecentSummary {
    let since = now - Duration::days(RECENT_WINDOW_DAYS);

    let mut tasks_created: u64 = 0;
    let mut tasks_completed: u64 = 0;
    for task in tasks {
        if task.created_at >= since {
            tasks_created = tasks_created.saturating_add(1);
        }
        if matches!(task.status, TaskStatus::Done | TaskStatus::Archived)
            && task_done_at(task).is_some_and(|done_at| done_at >= since)
        {
            tasks_completed = tasks_completed.saturating_add(1);
        }
    }

    let mut tool_calls_by_surface: BTreeMap<String, u64> = BTreeMap::new();
    for row in inputs.audit_tool_calls_by_surface_recent {
        *tool_calls_by_surface
            .entry(row.surface.clone())
            .or_insert(0) += row.total;
    }

    let workflows_run: u64 = inputs
        .job_runs
        .iter()
        .filter(|run| run.state == JobRunState::Success)
        .filter(|run| run_completed_at(run) >= since)
        .count() as u64;

    RecentSummary {
        since: since.to_rfc3339(),
        tasks_created,
        tasks_completed,
        tool_calls_by_surface,
        workflows_run,
    }
}

/// Best-effort timestamp for when a task entered `done`/`archived`.
/// Task history is no longer embedded in the public task DTO, so summary
/// generation uses the envelope `updated_at` timestamp.
pub(super) fn task_done_at(task: &Task) -> Option<DateTime<Utc>> {
    Some(task.updated_at)
}

/// Best-effort completion timestamp for a JobRun. `finished_at` is set when
/// the run terminates; the fallback to `created_at` keeps the recency
/// filter conservative for legacy rows that pre-date that field.
fn run_completed_at(run: &JobRun) -> DateTime<Utc> {
    run.finished_at.unwrap_or(run.created_at)
}
