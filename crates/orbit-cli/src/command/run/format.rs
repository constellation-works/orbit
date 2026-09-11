use orbit_types::workflow::{JobRunState, PipelineState, run_id_role};

/// Which side of a parent/child relationship a run id declares.
///
/// Sibling top-level runs and a run's own children share a minute stem, so the
/// marked sequence in the id is what tells them apart [ORB-12111]. An id minted
/// before the markers existed reads as unmarked rather than being assigned a
/// role its suffix never encoded.
pub(crate) fn format_run_role(run_id: &str) -> String {
    run_id_role(run_id).map_or_else(|| "unmarked".to_string(), |role| role.to_string())
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

pub(crate) fn format_duration(value: Option<u64>) -> String {
    value
        .map(|duration| format!("{duration}ms"))
        .unwrap_or_else(|| "-".to_string())
}

pub(crate) fn format_waiting_line(
    run_state: JobRunState,
    state: Option<&PipelineState>,
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
    let limit = state?.drain_worker_limit.as_ref()?;
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

/// [ORB-11283] Whether this drain was told to stop new admissions.
///
/// Printed whenever the control is present, including on a finished run: that
/// is how an operator distinguishes a drain that wound down after `--stop`
/// from one that was cancelled.
pub(crate) fn format_admissions_stop_line(state: Option<&PipelineState>) -> Option<String> {
    let stop = state?.drain_admissions_stop.as_ref()?;
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
