//! `orbit run auto --approve-proposed`: which `proposed` tasks each drain
//! pass pilots for approval, and the durable report of what it approved and
//! held.
//!
//! Selection only decides who is worth a pilot. Approval itself happens in
//! the task-pilot apply boundary under the drain's verified authority, so the
//! qualification checked here is a cost filter, not the gate. A task the
//! pilot held is not piloted again until it changes: its hold marker stays
//! the newest entry in its history, and a pilot this drain already ran that
//! left no decision behind (a failed or stale pilot) is not retried until the
//! task changes either. Creation-grant rows are not changes here: every write
//! to a task holding a grant appends one after its own entry to re-seal it.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::task::{CONTEXT_CREATION_AUTHORIZED_EVENT, TaskStatus};
use orbit_types::workflow::{DrainApprovalReport, DrainWaitingTask, PipelineState};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::TaskListFilter;
use crate::runtime::engine::crew::normalized_task_crew;

use super::super::task_pilot::{approval_disqualification, approved_by_drain, held_classification};
use super::action_failed;
use super::drains::live_admissions_stop;

/// Most proposed tasks one pass pilots; the rest wait for the next pass.
const MAX_CANDIDATES_PER_PASS: usize = 10;
/// Bounded lists on the durable report; totals sit beside them.
const REPORTED_TASKS: usize = 20;
const PILOT_JOB: &str = "task_pilot_pipeline";

/// One qualifying proposed task.
#[derive(Debug, Clone)]
pub(super) struct ApprovalCandidate {
    pub(super) id: String,
    pub(super) crew: Option<String>,
}

/// The approval view of every `proposed` task in the workspace.
pub(super) struct ApprovalSnapshot {
    /// Qualifying tasks with no current hold, oldest first.
    pub(super) candidates: Vec<ApprovalCandidate>,
    pub(super) held: Vec<DrainWaitingTask>,
}

impl ApprovalSnapshot {
    pub(super) fn held_by_reason(&self) -> BTreeMap<String, u64> {
        self.held.iter().fold(BTreeMap::new(), |mut counts, task| {
            *counts
                .entry(task.reason.clone().unwrap_or_default())
                .or_default() += 1;
            counts
        })
    }
}

/// Classify the workspace's `proposed` tasks for the drain `drain_run_id`.
pub(super) fn approval_snapshot(
    runtime: &OrbitRuntime,
    drain_run_id: Option<&str>,
) -> Result<ApprovalSnapshot, OrbitError> {
    let mut proposed = runtime
        .task_candidates(
            &TaskListFilter {
                statuses: Some(vec![TaskStatus::Proposed]),
                ..TaskListFilter::default()
            },
            usize::MAX,
        )?
        .items;
    proposed.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then(left.id.cmp(&right.id))
    });
    let attempts = drain_run_id
        .map(|run_id| pilot_attempts(runtime, run_id))
        .transpose()?
        .unwrap_or_default();
    let mut snapshot = ApprovalSnapshot {
        candidates: Vec::new(),
        held: Vec::new(),
    };
    for task in proposed {
        let held = |reason: &str, detail: Option<String>| DrainWaitingTask {
            task_id: task.id.clone(),
            reason: Some(reason.to_string()),
            blocked_by: Vec::new(),
            detail,
        };
        if let Some(reason) =
            approval_disqualification(&task.tags, &task.context_files, task.complexity)
        {
            snapshot.held.push(held(reason, None));
            continue;
        }
        let history = runtime.get_task_history(&task.id)?;
        let latest = history
            .iter()
            .rev()
            .find(|entry| entry.event != CONTEXT_CREATION_AUTHORIZED_EVENT);
        if let Some(classification) = latest
            .filter(|entry| entry.event == "task_pilot_applied")
            .and_then(|entry| entry.note.as_deref())
            .and_then(held_classification)
        {
            snapshot.held.push(held(classification, None));
            continue;
        }
        let changed_at = latest.map(|entry| entry.at);
        if let Some(run_id) = attempts.iter().find_map(|attempt| {
            (attempt.task_ids.contains(&task.id)
                && changed_at.is_none_or(|at| attempt.submitted_at > at))
            .then_some(attempt.run_id.as_str())
        }) {
            snapshot.held.push(held(
                "pilot_unresolved",
                Some(format!(
                    "task-pilot run {run_id} left no decision; edit the task or approve it to retry"
                )),
            ));
            continue;
        }
        snapshot.candidates.push(ApprovalCandidate {
            id: task.id,
            crew: normalized_task_crew(task.crew.as_deref()),
        });
    }
    Ok(snapshot)
}

struct PilotAttempt {
    run_id: String,
    task_ids: BTreeSet<String>,
    submitted_at: DateTime<Utc>,
}

/// The finished task-pilot children this drain dispatched.
fn pilot_attempts(runtime: &OrbitRuntime, run_id: &str) -> Result<Vec<PilotAttempt>, OrbitError> {
    let Some(state) = runtime.stores().jobs().read_run_state(run_id)? else {
        return Ok(Vec::new());
    };
    let mut attempts = Vec::new();
    for child in state
        .child_dispatches
        .iter()
        .filter(|child| child.job_name == PILOT_JOB)
    {
        let Some(run) = runtime.stores().jobs().get_job_run(&child.child_run_id)? else {
            continue;
        };
        if !run.state.is_terminal() {
            continue;
        }
        let task_ids = run
            .input
            .as_ref()
            .and_then(|input| input.get("task_ids"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        attempts.push(PilotAttempt {
            run_id: run.run_id,
            task_ids,
            submitted_at: child.submitted_at,
        });
    }
    Ok(attempts)
}

/// The tasks this drain approved, read back from the approve transition's
/// history note rather than from the pilot's output.
fn approved_by(runtime: &OrbitRuntime, run_id: &str, task_ids: &[String]) -> Vec<String> {
    task_ids
        .iter()
        .filter(|task_id| {
            runtime
                .get_task(task_id)
                .is_ok_and(|task| task.status != TaskStatus::Proposed)
                && runtime.get_task_history(task_id).is_ok_and(|history| {
                    history.iter().any(|entry| {
                        entry.event == "proposal_approved"
                            && entry
                                .note
                                .as_deref()
                                .is_some_and(|note| approved_by_drain(note, run_id))
                    })
                })
        })
        .cloned()
        .collect()
}

/// Replace the held view and add `approved` to the drain's durable report.
/// Best effort, like the admission pass record: a drain that cannot write its
/// report must still approve and admit work.
fn record_report(
    runtime: &OrbitRuntime,
    run_id: &str,
    snapshot: &ApprovalSnapshot,
    approved: &[String],
) {
    let held_by_reason = snapshot.held_by_reason();
    let result =
        runtime
            .stores()
            .jobs()
            .update_run_state(run_id, &mut |_, state: &mut PipelineState| {
                let report = state
                    .drain_approvals
                    .get_or_insert_with(DrainApprovalReport::default);
                report.recorded_at = Some(Utc::now());
                report.approved_total += approved.len() as u64;
                report.approved.extend(approved.iter().cloned());
                let excess = report.approved.len().saturating_sub(REPORTED_TASKS);
                report.approved.drain(..excess);
                report.held = snapshot.held.iter().take(REPORTED_TASKS).cloned().collect();
                report.held_total = snapshot.held.len() as u64;
                report.held_by_reason = held_by_reason.clone();
                Ok(())
            });
    if let Err(error) = result {
        tracing::warn!(
            target: "orbit.core.job_run",
            run_id,
            %error,
            "drain could not record its proposed-task approvals; run show will not list them"
        );
    }
}

fn drain_run_id(input: &Value) -> Option<&str> {
    input
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn held_json(snapshot: &ApprovalSnapshot) -> Value {
    json!({
        "held": snapshot.held.iter().take(REPORTED_TASKS).collect::<Vec<_>>(),
        "held_total": snapshot.held.len(),
        "held_by_reason": snapshot.held_by_reason(),
    })
}

/// One pass's selection: the qualifying `proposed` tasks to pilot now, and
/// the drain-scoped authority record the pilot's apply step verifies.
pub(in super::super) fn select_proposed_approvals(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let enabled = match input.get("approve_proposed") {
        None | Some(Value::Null) => false,
        Some(value) => value.as_bool().ok_or_else(|| {
            action_failed(action, "approve_proposed must be a boolean".to_string())
        })?,
    };
    let run_id = drain_run_id(input);
    let (Some(run_id), true) = (run_id, enabled) else {
        return Ok(json!({
            "approve_proposed": false,
            "task_ids": [],
            "candidate_count": 0,
            "drain_promotion": Value::Null,
        }));
    };
    let snapshot = approval_snapshot(runtime, Some(run_id))
        .map_err(|error| action_failed(action, format!("read proposed tasks: {error}")))?;
    record_report(runtime, run_id, &snapshot, &[]);
    // A stopped drain admits nothing, so it approves nothing either.
    let admissions_stopped = live_admissions_stop(runtime, input).is_some();
    let task_ids = if admissions_stopped {
        Vec::new()
    } else {
        match snapshot.candidates.first() {
            None => Vec::new(),
            Some(first) => {
                let target_crew = &first.crew;
                snapshot
                    .candidates
                    .iter()
                    .filter(|candidate| &candidate.crew == target_crew)
                    .take(MAX_CANDIDATES_PER_PASS)
                    .map(|candidate| candidate.id.clone())
                    .collect::<Vec<_>>()
            }
        }
    };
    let mut output = held_json(&snapshot);
    output["approve_proposed"] = json!(true);
    output["admissions_stopped"] = json!(admissions_stopped);
    output["candidate_count"] = json!(task_ids.len());
    output["deferred_candidates"] = json!(snapshot.candidates.len() - task_ids.len());
    output["task_ids"] = json!(task_ids);
    output["drain_promotion"] = json!({ "run_id": run_id });
    Ok(output)
}

/// After the pass's pilot: which of its tasks this drain approved, and the
/// refreshed held view.
pub(in super::super) fn record_proposed_approvals(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let run_id = drain_run_id(input)
        .ok_or_else(|| action_failed(action, "the drain run id is unavailable".to_string()))?;
    let task_ids = input
        .get("task_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| action_failed(action, "task_ids must be an array".to_string()))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| action_failed(action, "task_ids must contain strings".to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let approved = approved_by(runtime, run_id, &task_ids);
    let snapshot = approval_snapshot(runtime, Some(run_id))
        .map_err(|error| action_failed(action, format!("read proposed tasks: {error}")))?;
    record_report(runtime, run_id, &snapshot, &approved);
    let mut output = held_json(&snapshot);
    output["approved"] = json!(approved);
    output["approved_count"] = json!(approved.len());
    output["pilot_run_id"] = input["pilot"]["run_id"].clone();
    output["pilot_status"] = input["pilot"]["status"].clone();
    Ok(output)
}

/// Readiness's view of `--approve-proposed` for the status drain: what it
/// has approved so far and why each remaining proposed task is held. A drain
/// started without the flag reports only `enabled: false`.
pub(super) fn readiness_approvals(
    runtime: &OrbitRuntime,
    drain_run_id: Option<&str>,
    drain_input: Option<&Value>,
) -> Result<Value, OrbitError> {
    let enabled = drain_input
        .and_then(|input| input.get("approve_proposed"))
        .and_then(Value::as_bool)
        == Some(true);
    let Some(run_id) = drain_run_id.filter(|_| enabled) else {
        return Ok(json!({ "enabled": false }));
    };
    let report = runtime
        .stores()
        .jobs()
        .read_run_state(run_id)?
        .and_then(|state| state.drain_approvals)
        .unwrap_or_default();
    let snapshot = approval_snapshot(runtime, Some(run_id))?;
    let mut value = held_json(&snapshot);
    value["enabled"] = json!(true);
    value["drain_run_id"] = json!(run_id);
    value["approved_total"] = json!(report.approved_total);
    value["approved"] = json!(report.approved);
    value["awaiting_pilot"] = json!(snapshot.candidates.len());
    Ok(value)
}
