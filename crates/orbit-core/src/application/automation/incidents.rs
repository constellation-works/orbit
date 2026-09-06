//! Bounded authoritative run lineage and current task-intent evidence.
use crate::{
    OrbitRuntime,
    application::job::{RunOwnerLiveness, run_owner_liveness},
};
use orbit_automation::{
    AutomationError,
    members::incidents::{IncidentFacts, incident_key},
};
use orbit_types::{
    task::{Task, TaskStatus},
    workflow::{JobRun, JobRunState},
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub(crate) fn failure_coupled(
    runtime: &OrbitRuntime,
    task: &Task,
    run_id: &str,
) -> Result<bool, AutomationError> {
    if task.status != TaskStatus::Blocked || task.job_run_id.as_deref() != Some(run_id) {
        return Ok(false);
    }
    let history = runtime.get_task_history(&task.id)?;
    let last = history.iter().rev().find(|entry| entry.to_status.is_some());
    Ok(last.is_some_and(|entry| {
        entry.event == "workflow_run_failed"
            && entry.to_status == Some(TaskStatus::Blocked)
            && entry
                .note
                .as_deref()
                .is_some_and(|note| note.contains(&format!("run_id={run_id},")))
    }))
}
fn root(runtime: &OrbitRuntime, run: &JobRun) -> Result<String, AutomationError> {
    let mut current = run.clone();
    let mut seen = BTreeSet::new();
    for _ in 0..50 {
        if !seen.insert(current.run_id.clone()) {
            return Err(AutomationError::Deferred("incident_lineage_cycle".into()));
        }
        let Some(parent) = &current.retry_source_run_id else {
            return Ok(current.run_id);
        };
        current = runtime
            .get_job_run_backend(parent)?
            .ok_or_else(|| AutomationError::Deferred("incident_lineage_missing".into()))?;
    }
    Err(AutomationError::Deferred("incident_lineage_budget".into()))
}
pub(crate) fn observe(
    runtime: &OrbitRuntime,
    task: &Task,
) -> Result<(String, Value), AutomationError> {
    observe_with_settlement(runtime, task, true)
}
fn observe_with_settlement(
    runtime: &OrbitRuntime,
    task: &Task,
    require_settled: bool,
) -> Result<(String, Value), AutomationError> {
    let run_id = task
        .job_run_id
        .as_deref()
        .ok_or_else(|| AutomationError::Deferred("human_block".into()))?;
    let mut run = runtime.show_job_run(run_id)?;
    let coupled = failure_coupled(runtime, task, run_id)?;
    let diagnostic_origin = run.job_id == "task_triage_pipeline"
        || run
            .input
            .as_ref()
            .and_then(|v| v.get("automation_origin"))
            .and_then(Value::as_str)
            == Some("triage");
    let cancelled = run.state == JobRunState::Cancelled;
    let failed = matches!(run.state, JobRunState::Failed | JobRunState::Timeout);
    let mut path_settled = true;
    let mut diagnostic_origin = diagnostic_origin;
    let mut episode = root(runtime, &run)?;
    // A blocking dispatch with a typed terminal child result is explicit
    // causality. Multiple failing children need separate obligations; ambiguous
    // wrapper evidence is withheld instead of guessing from error prose.
    let mut seen = BTreeSet::new();
    for _ in 0..50 {
        if !seen.insert(run.run_id.clone()) {
            return Err(AutomationError::Deferred("incident_lineage_cycle".into()));
        }
        path_settled &=
            run.state.is_terminal() && run_owner_liveness(&run) == RunOwnerLiveness::Stopped;
        diagnostic_origin |= run.job_id == "task_triage_pipeline"
            || run
                .input
                .as_ref()
                .and_then(|v| v.get("automation_origin"))
                .and_then(Value::as_str)
                == Some("triage");
        let state = runtime.read_run_state(&run.run_id)?;
        path_settled &= state
            .as_ref()
            .is_none_or(|s| !s.child_dispatches.iter().any(|d| d.phase.is_open()));
        let children: Vec<_> = state
            .as_ref()
            .map(|s| {
                s.child_dispatches
                    .iter()
                    .filter(|d| {
                        d.blocking
                            && matches!(d.child_status.as_deref(), Some("failed" | "timeout"))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if children.len() > 1 {
            return Err(AutomationError::Deferred("incident_unresolved".into()));
        }
        let Some(child) = children.first() else {
            break;
        };
        run = runtime.show_job_run(&child.child_run_id)?;
        episode = root(runtime, &run)?;
    }
    if seen.len() == 50 {
        return Err(AutomationError::Deferred("incident_lineage_budget".into()));
    }
    let root_run = runtime
        .get_job_run_backend(&episode)?
        .ok_or_else(|| AutomationError::Deferred("incident_lineage_missing".into()))?;
    let mut related = vec![root_run];
    let mut lineage = BTreeSet::from([episode.clone()]);
    let mut index = 0;
    while index < related.len() {
        let children = runtime
            .stores()
            .jobs()
            .job_run_retries(&related[index].run_id, 51)?;
        if children.len() > 50 || related.len() + children.len() > 1000 {
            return Err(AutomationError::Deferred("incident_scan_budget".into()));
        }
        for child in children {
            if lineage.insert(child.run_id.clone()) {
                related.push(child);
            }
        }
        index += 1;
    }
    let settled = related
        .iter()
        .filter(|r| lineage.contains(&r.run_id))
        .all(|r| r.state.is_terminal() && run_owner_liveness(r) == RunOwnerLiveness::Stopped)
        && run_owner_liveness(&run) == RunOwnerLiveness::Stopped;
    let state = runtime.read_run_state(&run.run_id)?;
    let descendants_settled = state
        .as_ref()
        .is_none_or(|s| !s.child_dispatches.iter().any(|d| d.phase.is_open()));
    let cause = run
        .steps
        .iter()
        .find(|s| matches!(s.state, JobRunState::Failed | JobRunState::Timeout))
        .map(|step| format!("{}:{}:{}", episode, step.step_index, step.target_id));
    let key = incident_key(&IncidentFacts {
        workspace: runtime.workspace_id()?,
        episode: Some(episode.clone()),
        cause,
        failure: failed && matches!(run.state, JobRunState::Failed | JobRunState::Timeout),
        recovery_settled: !require_settled || (path_settled && settled && descendants_settled),
        current_failure_coupling: coupled,
        diagnostic_origin,
        cancellation: cancelled || run.state == JobRunState::Cancelled,
    })?;
    Ok((
        key,
        json!({"episode":episode,"cause_run_id":run.run_id,"coupled_run_id":run_id,
        "task_revision":task.updated_at, "task_id":task.id}),
    ))
}

/// Hydrate at most 1,000 blocked task envelopes to find the complete current
/// cohort, including tasks coupled to different wrappers of the same cause.
/// An incomplete inventory never certifies partial incident coverage.
pub(crate) fn members(
    runtime: &OrbitRuntime,
) -> Result<std::collections::BTreeMap<String, Vec<String>>, AutomationError> {
    let candidates = runtime.task_candidates(
        &orbit_store::contracts::TaskListFilter {
            statuses: Some(vec![TaskStatus::Blocked]),
            ..Default::default()
        },
        1001,
    )?;
    if candidates.total > 1000 {
        return Err(AutomationError::Deferred(
            "incident_inventory_budget".into(),
        ));
    }
    let mut members: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    let mut unsettled = BTreeSet::new();
    for candidate in candidates.items {
        let task = runtime.get_task(&candidate.id)?;
        if let Ok((key, _)) = observe(runtime, &task) {
            members.entry(key).or_default().push(task.id);
        } else if let Ok((key, _)) = observe_with_settlement(runtime, &task, false) {
            unsettled.insert(key);
        }
    }
    members.retain(|key, _| !unsettled.contains(key));
    for ids in members.values_mut() {
        ids.sort();
    }
    Ok(members)
}
