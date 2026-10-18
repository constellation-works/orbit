use orbit_types::workflow::{
    JobRunState, JobRunTrigger, JobRunTriggerKind, PipelineState, run_id_role,
};
use serde::Serialize;
use serde_json::Value;

/// One text vocabulary for readiness payloads and recorded drain waits.
/// Persisted reason codes and JSON projections remain unchanged.
pub(crate) fn waiting_reason(reason: &str) -> &str {
    match reason {
        "dependency_not_done" => "unmet_dependency",
        other => other,
    }
}

/// Text-only blocker details. Statuses are read once per command; older
/// checkpoints and unavailable tasks still render the IDs they recorded.
pub(crate) struct WaitingTaskFormatter {
    statuses: std::collections::BTreeMap<String, orbit_types::task::TaskStatus>,
}

impl WaitingTaskFormatter {
    pub(crate) fn new(runtime: &orbit_core::OrbitRuntime) -> Self {
        Self {
            statuses: runtime.task_status_index().unwrap_or_else(|error| {
                tracing::warn!(%error, "waiting task statuses unavailable");
                Default::default()
            }),
        }
    }

    fn task_label(&self, id: &str, status: Option<&str>) -> String {
        let status = status
            .map(str::to_string)
            .or_else(|| self.statuses.get(id).map(ToString::to_string));
        match status {
            Some(status) => format!("{id} ({status})"),
            None => id.to_string(),
        }
    }

    /// Readiness has conflicts and dependencies; drain checkpoints have only
    /// `blocked_by`. Prefer the richer snapshot without repeating its IDs.
    pub(crate) fn blockers(&self, task: &Value) -> String {
        let reason = waiting_reason(task["reason"].as_str().unwrap_or("unknown"));
        let dependencies = task["dependencies"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|dependency| {
                let id = dependency["task_id"].as_str()?;
                Some(self.task_label(id, dependency["status"].as_str()))
            })
            .collect::<Vec<_>>();
        if !dependencies.is_empty() {
            return format!(" waits-on {}", dependencies.join(", "));
        }

        // Preserve payload order, group files by holder, and count additional
        // distinct files per holder rather than hiding other blocking tasks.
        let mut holders: Vec<(&str, Vec<&str>)> = Vec::new();
        for conflict in task["conflicts"].as_array().into_iter().flatten() {
            let Some(id) = conflict["locking_task_id"].as_str() else {
                continue;
            };
            let index = match holders.iter().position(|(holder, _)| *holder == id) {
                Some(index) => index,
                None => {
                    holders.push((id, Vec::new()));
                    holders.len() - 1
                }
            };
            if let Some(file) = conflict["requested_file"].as_str()
                && !holders[index].1.contains(&file)
            {
                holders[index].1.push(file);
            }
        }
        let mut labels = holders
            .iter()
            .map(|(id, files)| {
                let mut label = self.task_label(id, None);
                if let Some(first) = files.first() {
                    label.push_str(&format!(" on {first}"));
                    if files.len() > 1 {
                        label.push_str(&format!(" +{} more", files.len() - 1));
                    }
                }
                label
            })
            .collect::<Vec<_>>();
        for id in task["blocking_task_ids"]
            .as_array()
            .or_else(|| task["blocked_by"].as_array())
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !holders.iter().any(|(holder, _)| *holder == id) {
                labels.push(self.task_label(id, None));
            }
        }
        if labels.is_empty() {
            String::new()
        } else if reason == "unmet_dependency" {
            format!(" waits-on {}", labels.join(", "))
        } else {
            format!(" blocked-by={}", labels.join(", "))
        }
    }
}

/// A failed run with no failed descendant, in child-dispatch traversal order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct RunRootCause {
    pub(crate) run_id: String,
    pub(crate) step: Option<String>,
    pub(crate) message: Option<String>,
}

pub(crate) fn format_root_cause_lines(causes: &[RunRootCause]) -> Vec<String> {
    causes
        .iter()
        .enumerate()
        .map(|(index, cause)| {
            let label = if index == 0 {
                "Root cause:"
            } else {
                "Additional root cause:"
            };
            let mut lines = format!(
                "{label} run={} step={} error={}",
                cause.run_id,
                cause.step.as_deref().unwrap_or("-"),
                cause.message.as_deref().unwrap_or("-"),
            );
            lines.push_str(&format!("\nLogs: orbit run logs {}", cause.run_id));
            if let Some(step) = cause.step.as_deref() {
                lines.push_str(&format!(" -s {step}"));
            }
            lines
        })
        .collect()
}

/// Which side of a parent/child relationship a run id declares.
///
/// Sibling top-level runs and a run's own children share a minute stem, so the
/// marked sequence in the id is what tells them apart [ORB-12111]. An id minted
/// before the markers existed reads as unmarked rather than being assigned a
/// role its suffix never encoded.
pub(crate) fn format_run_role(run_id: &str) -> String {
    run_id_role(run_id).map_or_else(|| "unmarked".to_string(), |role| role.to_string())
}

/// ROLE column for `run history`: a routine-fired run names the routine
/// instead of the id marker (`top-level` / `unmarked`) [ORB-12255].
pub(crate) fn format_history_role(run_id: &str, trigger: Option<&JobRunTrigger>) -> String {
    if let Some(trigger) = trigger
        && trigger.kind == JobRunTriggerKind::Routine
    {
        return trigger
            .routine
            .as_deref()
            .filter(|name| !name.is_empty())
            .unwrap_or("routine")
            .to_string();
    }
    format_run_role(run_id)
}

pub(crate) fn summarize_error_message(raw: Option<&str>) -> String {
    let value = raw.unwrap_or("-").replace('\n', " ");
    if value.chars().count() <= 120 {
        return value;
    }
    let truncated = value.chars().take(120).collect::<String>();
    format!("{truncated}...")
}

pub(crate) fn format_timestamp(value: Option<chrono::DateTime<chrono::Utc>>) -> String {
    value
        .map(|v| v.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| "-".to_string())
}

/// A duration for human output: `3h 39m 51s`, or `250ms` under a second.
///
/// Leading zero units are dropped and inner ones kept (`1h 0m 5s`), so every
/// value reads as one span. Sub-second remainders of longer spans are
/// truncated; `--json` carries the exact milliseconds.
pub(crate) fn format_duration(value: Option<u64>) -> String {
    let Some(ms) = value else {
        return "-".to_string();
    };
    if ms < 1000 {
        return format!("{ms}ms");
    }
    let total_seconds = ms / 1000;
    let (hours, minutes, seconds) = (
        total_seconds / 3600,
        total_seconds % 3600 / 60,
        total_seconds % 60,
    );
    match (hours, minutes) {
        (0, 0) => format!("{seconds}s"),
        (0, _) => format!("{minutes}m {seconds}s"),
        _ => format!("{hours}h {minutes}m {seconds}s"),
    }
}

/// The tasks holding each selector a run is waiting on, keyed by selector.
pub(crate) type LockHolders = std::collections::BTreeMap<String, Vec<String>>;

pub(crate) fn format_waiting_line(
    run_state: JobRunState,
    state: Option<&PipelineState>,
) -> Option<String> {
    format_waiting_line_with_holders(run_state, state, &LockHolders::new())
}

/// [`format_waiting_line`], naming the task that holds each lock when known.
pub(crate) fn format_waiting_line_with_holders(
    run_state: JobRunState,
    state: Option<&PipelineState>,
    holders: &LockHolders,
) -> Option<String> {
    if run_state.is_terminal() {
        return None;
    }
    let state = state?;
    let deps = state
        .waiting_on_deps
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .collect::<Vec<_>>();
    let locks = state
        .waiting_on_locks
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(|selector| match holders.get(selector) {
            Some(tasks) if !tasks.is_empty() => {
                format!("{selector} (held by {})", tasks.join(","))
            }
            _ => selector.to_string(),
        })
        .collect::<Vec<_>>();

    let mut parts = Vec::new();
    if !deps.is_empty() {
        parts.push(format!("deps: {}", deps.join(", ")));
    }
    if !locks.is_empty() {
        parts.push(format!("locks: {}", locks.join(", ")));
    }
    (!parts.is_empty()).then(|| format!("Waiting on {}", parts.join("; ")))
}

/// One line per child Run this run dispatched [ORB-10971].
///
/// Unlike the waiting line above this is not filtered by the parent's state:
/// the whole point of the dispatch checkpoint is that an operator staring at a
/// stalled — or cancelled — parent can name its child immediately, so the
/// lineage is printed for terminal runs too.
///
/// [ORB-11253] The worker ceiling in force on a drain, and who last moved it.
///
/// Printed only when an operator has retuned the run: an untouched drain is
/// admitting under the `max_active_leaf_runs` its input already shows, so a
/// line restating it would be noise.
pub(crate) fn format_worker_limit_line(state: Option<&PipelineState>) -> Option<String> {
    let limit = state?.pull_drain.drain_worker_limit.as_ref()?;
    let mut line = format!(
        "Workers: {} (was {}, revision {}, set by {})",
        limit.max_active_leaf_runs,
        limit.previous_max_active_leaf_runs,
        limit.revision,
        limit.actor,
    );
    if let Some(reason) = &limit.reason {
        line.push_str(&format!(" reason={reason}"));
    }
    Some(line)
}

/// [ORB-15383] The deadline an operator extended this drain's window to.
///
/// Printed only once an extension moved the deadline: an unextended drain
/// still runs to the deadline its own window step stamped.
pub(crate) fn format_window_extension_line(state: Option<&PipelineState>) -> Option<String> {
    let extension = state?.pull_drain.drain_window_extension.as_ref()?;
    let mut line = format!(
        "Window: extended to {} (was {}, revision {}, by {})",
        extension.deadline.format("%Y-%m-%dT%H:%M:%SZ"),
        extension.previous_deadline.format("%Y-%m-%dT%H:%M:%SZ"),
        extension.revision,
        extension.actor,
    );
    if let Some(reason) = &extension.reason {
        line.push_str(&format!(" reason={reason}"));
    }
    Some(line)
}

/// [ORB-11283] Whether this drain was told to stop new admissions.
///
/// Printed whenever the control is present, including on a finished run: that
/// is how an operator distinguishes a drain that wound down after `--stop`
/// from one that was cancelled.
pub(crate) fn format_admissions_stop_line(state: Option<&PipelineState>) -> Option<String> {
    let stop = state?.pull_drain.drain_admissions_stop.as_ref()?;
    let mut line = format!(
        "Admissions: stopped by {} at {}",
        stop.actor,
        stop.stopped_at.format("%Y-%m-%dT%H:%M:%SZ"),
    );
    if let Some(reason) = &stop.reason {
        line.push_str(&format!(" reason={reason}"));
    }
    Some(line)
}

pub(crate) fn format_child_dispatch_lines(state: Option<&PipelineState>) -> Vec<String> {
    let Some(state) = state else {
        return Vec::new();
    };
    state
        .child_dispatches
        .iter()
        .map(|dispatch| {
            let mut line = format!(
                "Child {} job={} step={} phase={} queued={}",
                dispatch.child_run_id,
                dispatch.job_name,
                dispatch.parent_step_id.as_deref().unwrap_or("-"),
                dispatch.phase.as_str(),
                dispatch.queued,
            );
            if let Some(status) = &dispatch.child_status {
                line.push_str(&format!(" status={status}"));
            }
            if let Some(cancellation) = &dispatch.cancellation {
                line.push_str(&format!(
                    " cancellation={}/{}",
                    cancellation.policy.as_str(),
                    cancellation.outcome
                ));
            }
            if let Some(error) = &dispatch.error {
                line.push_str(&format!(" error={}", summarize_error_message(Some(error))));
            }
            line
        })
        .collect()
}

/// Explain how an auto drain chose this run's crew, including the odds it drew
/// against [ORB-12604].
///
/// Absent for manually dispatched runs, which record no selection. A pool
/// admitted before weights existed lists plain names, and is rendered as the
/// bare names it was stored as.
pub(crate) fn format_crew_selection_line(input: Option<&serde_json::Value>) -> Option<String> {
    let selection = input?.get("crew_selection")?.as_object()?;
    let text = |key: &str| {
        selection
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("-")
    };
    let pool = selection
        .get("eligible_pool")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .map(|entry| match entry {
            serde_json::Value::String(name) => name.clone(),
            entry => {
                let name = entry
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("-");
                match entry.get("weight").and_then(serde_json::Value::as_u64) {
                    Some(weight) => format!("{name}:{weight}"),
                    None => name.to_string(),
                }
            }
        })
        .collect::<Vec<_>>();
    let mut line = format!(
        "Crew Selection: {} source={} complexity={}",
        text("crew"),
        text("source"),
        text("complexity"),
    );
    if !pool.is_empty() {
        line.push_str(&format!(" eligible={}", pool.join(", ")));
    }
    Some(line)
}

/// Show backlog admission exclusions retained in the pipeline checkpoint.
///
/// The structured state remains the source of truth; this projection is only
/// for the human-readable `orbit run show` view. Older or partially written
/// checkpoints are ignored so inspection remains available when the optional
/// diagnostic data is absent.
pub(crate) fn format_backlog_exclusion_lines(state: Option<&PipelineState>) -> Vec<String> {
    let Some(excluded) = state
        .and_then(|state| state.pipeline.get("list_backlog"))
        .and_then(|step| step.get("excluded"))
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };

    let lines = excluded
        .iter()
        .filter_map(|entry| {
            let task_id = entry.get("id").and_then(serde_json::Value::as_str)?;
            let reason = entry
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            let crew = entry
                .get("crew")
                .and_then(serde_json::Value::as_str)
                .map(|crew| format!(" crew={crew}"))
                .unwrap_or_default();
            let conflicts = entry
                .get("conflicts")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|conflict| {
                    conflict
                        .get("locking_task_id")
                        .and_then(serde_json::Value::as_str)
                })
                .collect::<Vec<_>>();
            let blocked_by = if conflicts.is_empty() {
                String::new()
            } else {
                format!(" blocked-by={}", conflicts.join(","))
            };
            Some(format!(
                "Excluded task {task_id}: {reason}{crew}{blocked_by}"
            ))
        })
        .collect::<Vec<_>>();

    if lines.is_empty() {
        return Vec::new();
    }

    let mut output = vec![format!("Excluded backlog tasks ({}):", lines.len())];
    output.extend(lines);
    output
}
