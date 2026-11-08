//! Inspect, await and record drain pilot children and approval reports.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_store::contracts::JobRunQuery;
use orbit_types::task::{CONTEXT_CREATION_AUTHORIZED_EVENT, Task, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{
    ChildDispatchPhase, DrainApprovalReport, JobRunState, PipelineState, ProviderFailureClass,
};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::contracts::INSPECTION_CREW_UNAVAILABLE;
use crate::application::task::{
    PILOT_WARNING_TRIAGED_EVENT, SYSTEM_ACTOR_LABEL, TaskRecordUpdateParams, TaskRecords,
};

use super::classify::templated_u64;
use super::drains::{drain_run_id, live_admissions_stop};
use crate::application::task::pilot::{
    ExplicitAssessmentReuse, UNRESOLVED_PILOT_EVENT, approved_by_drain,
    classify_explicit_assessment_reuse, held_classification, unresolved_pilot_entry,
    unresolved_pilot_run_id,
};

use super::approvals::{
    ApprovalSnapshot, MAX_PILOT_WAIT_SECONDS, PILOT_JOB, PILOT_WAIT_POLL, REPORTED_TASKS,
    approval_snapshot, held_json, proposed_tasks,
};

/// A live task-pilot run that names `task_id` in its input, or whose
/// reservation or prepare checkpoint is among `holders`.
///
/// Stored rows only: the reconciling job list resolves the job's catalog
/// entry and can finalize another run, and selection must do neither.
pub(super) fn live_pilot_run(
    runtime: &OrbitRuntime,
    holders: &BTreeMap<String, BTreeSet<String>>,
    task_id: &str,
) -> Result<Option<String>, OrbitError> {
    let named = runtime
        .stores()
        .jobs()
        .list_job_runs_filtered(&JobRunQuery {
            job_id: Some(PILOT_JOB.to_string()),
            task_id: Some(task_id.to_string()),
            states: vec![
                JobRunState::Pending,
                JobRunState::Running,
                JobRunState::Retrying,
            ],
            limit: Some(1),
            include_steps: false,
            ..JobRunQuery::default()
        })?;
    Ok(named
        .into_iter()
        .next()
        .map(|run| run.run_id)
        .or_else(|| holders.get(task_id)?.iter().next().cloned()))
}

/// One task-pilot child this drain dispatched.
pub(super) struct PilotChild {
    pub(super) run_id: String,
    /// The tasks it was asked to pilot, from its own run input.
    pub(super) task_ids: Vec<String>,
    pub(super) submitted_at: DateTime<Utc>,
    /// Its terminal state; `None` while it is pending or running.
    finished: Option<JobRunState>,
    /// Whether the drain still has to record what it approved. Recording
    /// closes the dispatch, so each pilot is counted once.
    unrecorded: bool,
    /// Whether a step failed for a task-independent reason, such as a provider
    /// outage or missing inspection-capable crew. Such a pilot says nothing
    /// about its tasks, so it is never recorded against them.
    task_independent_failure: bool,
}

impl PilotChild {
    pub(super) fn is_live(&self) -> bool {
        self.finished.is_none()
    }

    pub(super) fn awaits_recording(&self) -> bool {
        self.unrecorded && !self.is_live()
    }
}

/// The task-pilot children this drain dispatched, live and finished.
pub(super) fn pilot_children(
    runtime: &OrbitRuntime,
    run_id: &str,
) -> Result<Vec<PilotChild>, OrbitError> {
    let Some(state) = runtime.stores().jobs().read_run_state(run_id)? else {
        return Ok(Vec::new());
    };
    let mut pilots = Vec::new();
    for child in state
        .child_dispatches
        .iter()
        .filter(|child| child.job_name == PILOT_JOB)
    {
        let Some(run) = runtime.stores().jobs().get_job_run(&child.child_run_id)? else {
            continue;
        };
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
        let task_independent_failure = run.steps.iter().any(|step| {
            is_task_independent_pilot_failure(
                step.error_code.as_deref(),
                step.error_message.as_deref(),
            )
        });
        pilots.push(PilotChild {
            run_id: run.run_id,
            task_ids,
            submitted_at: child.submitted_at,
            finished: run.state.is_terminal().then_some(run.state),
            unrecorded: child.phase.is_open(),
            task_independent_failure,
        });
    }
    Ok(pilots)
}

/// Whether `task`'s latest certified assessment is a fresh explicit or
/// routine receipt this drain should approve without another pilot.
pub(super) fn reusable_explicit_promotion(
    runtime: &OrbitRuntime,
    task: &Task,
) -> Result<bool, OrbitError> {
    Ok(matches!(
        classify_explicit_assessment_reuse(runtime, task)?,
        ExplicitAssessmentReuse::Promote { .. }
    ))
}

/// Whether `entry` leaves the question "what did the task's last change
/// decide?" unanswered: a context grant, the triage of a warning hold, or an
/// undecided-pilot record. None of these is a change to the task.
pub(super) fn is_decision_neutral(entry: &TaskHistoryEntry) -> bool {
    entry.event == CONTEXT_CREATION_AUTHORIZED_EVENT
        || entry.event == PILOT_WARNING_TRIAGED_EVENT
        || entry.event == UNRESOLVED_PILOT_EVENT
}

/// Whether an earlier unresolved-pilot marker names a failure the task could
/// not affect, so configuration can release the hold without a task edit.
pub(super) fn unresolved_pilot_failed_independently(
    runtime: &OrbitRuntime,
    entry: &TaskHistoryEntry,
) -> Result<bool, OrbitError> {
    let Some(run_id) = unresolved_pilot_run_id(entry) else {
        return Ok(false);
    };
    let Some(run) = runtime.stores().jobs().get_job_run(&run_id)? else {
        return Ok(false);
    };
    Ok(run.steps.iter().any(|step| {
        is_task_independent_pilot_failure(step.error_code.as_deref(), step.error_message.as_deref())
    }))
}

/// Whether a pilot step failed for a reason unrelated to the task's content.
/// The inspection-crew marker may appear inside a wrapped executor diagnostic.
fn is_task_independent_pilot_failure(
    error_code: Option<&str>,
    error_message: Option<&str>,
) -> bool {
    ProviderFailureClass::of(error_code, error_message).is_some()
        || error_message.is_some_and(|message| message.contains(INSPECTION_CREW_UNAVAILABLE))
}

/// Record, on each proposed task, a finished pilot of this drain that decided
/// nothing about it: no change to the task since the pilot was submitted and
/// no apply after it. A pilot that failed on a provider outage or missing
/// inspection-capable crew says nothing about the task, so it is left
/// unrecorded and the next drain retries it.
/// Best effort, like the report: a task that cannot be recorded is piloted
/// again by a later drain.
fn record_unresolved_pilots(runtime: &OrbitRuntime, pilots: &[PilotChild]) {
    for pilot in pilots
        .iter()
        .filter(|pilot| pilot.awaits_recording() && !pilot.task_independent_failure)
    {
        for task_id in &pilot.task_ids {
            if let Err(error) = record_unresolved_pilot(runtime, task_id, pilot) {
                tracing::warn!(
                    target: "orbit.core.job_run",
                    task_id = %task_id,
                    run_id = %pilot.run_id,
                    %error,
                    "drain could not record a task-pilot run that left no decision"
                );
            }
        }
    }
}

fn record_unresolved_pilot(
    runtime: &OrbitRuntime,
    task_id: &str,
    pilot: &PilotChild,
) -> Result<(), OrbitError> {
    let task = runtime.get_task(task_id)?;
    if task.status != TaskStatus::Proposed {
        return Ok(());
    }
    // A child that stood down did not apply the explicit assessment. Keep
    // its certified reuse or hold decision instead of recording an unrelated
    // unresolved-child marker against it. A stale (`changed_since_pilot`)
    // assessment is the exception: the child was dispatched to repilot it,
    // so its failure is a failed repilot and is recorded, holding the task
    // until its meaning changes again or an operator acts.
    match classify_explicit_assessment_reuse(runtime, &task)? {
        ExplicitAssessmentReuse::NotReusable => {}
        ExplicitAssessmentReuse::Hold {
            reason: "changed_since_pilot",
            ..
        } => {}
        _ => return Ok(()),
    }
    let history = runtime.get_task_history(task_id)?;
    let latest = history
        .iter()
        .rev()
        .find(|entry| !is_decision_neutral(entry));
    if latest.is_some_and(|entry| entry.at >= pilot.submitted_at) {
        // An assessment history entry newer than submission normally means
        // the pilot decided. The exception is an applied assessment with
        // no hold on a task still proposed: its approval did not complete,
        // so recording it unresolved is what stops the next pass piloting
        // it again [ORB-21172]. A fresh explicit or routine assessment is
        // still proposed until this drain promotes it. Recording that
        // apply unresolved would hold the next pass before promotion
        // [ORB-21206].
        let unapproved_apply = latest.is_some_and(|entry| {
            entry.event == "task_pilot_applied" && held_classification(runtime, entry).is_none()
        });
        if !unapproved_apply || reusable_explicit_promotion(runtime, &task)? {
            return Ok(());
        }
    }
    runtime.stores().task_records().update(
        task_id,
        TaskRecordUpdateParams {
            actor: SYSTEM_ACTOR_LABEL.to_string(),
            append_history: vec![unresolved_pilot_entry(&pilot.run_id, Utc::now())],
            // Only a task still proposed takes the record: a write that moved
            // it on wins, and the pilot is then no longer this drain's concern.
            expected_status: Some(vec![TaskStatus::Proposed]),
            ..Default::default()
        },
    )?;
    Ok(())
}

pub(super) fn read_pilots(
    runtime: &OrbitRuntime,
    action: &str,
    run_id: &str,
) -> Result<Vec<PilotChild>, DispatchError> {
    pilot_children(runtime, run_id).map_err(|error| {
        DispatchError::action_failed(action, format!("read task-pilot children: {error}"))
    })
}

/// The tasks this drain approved, read back from the approve transition's
/// history decision rather than from the pilot's output.
fn approved_by(runtime: &OrbitRuntime, run_id: &str, task_ids: &[String]) -> Vec<String> {
    task_ids
        .iter()
        .filter(|task_id| {
            runtime
                .get_task(task_id)
                .is_ok_and(|task| task.status != TaskStatus::Proposed)
                && runtime.get_task_history(task_id).is_ok_and(|history| {
                    history.iter().any(|entry| {
                        entry.event == "proposal_approved" && approved_by_drain(entry, run_id)
                    })
                })
        })
        .cloned()
        .collect()
}

/// A finished pilot and the tasks this drain approved through it.
pub(super) struct RecordedPilot {
    run_id: String,
    status: JobRunState,
    approved: Vec<String>,
}

/// Replace the held view and add the pass's closed tasks and each finished
/// pilot's approvals to the drain's durable report. Recording a pilot closes
/// its dispatch in the same write, and a pilot already closed adds nothing,
/// so no approval is counted twice.
/// Best effort, like the admission pass record: a drain that cannot write its
/// report must still approve and admit work, and a pilot it failed to record
/// is recorded by a later pass.
pub(super) fn record_report(
    runtime: &OrbitRuntime,
    run_id: &str,
    snapshot: &ApprovalSnapshot,
    pilots: &[RecordedPilot],
    reused: &[String],
) {
    let closed = &snapshot.closed;
    let held_by_reason = snapshot.held_by_reason();
    let result =
        runtime
            .stores()
            .jobs()
            .update_run_state(run_id, &mut |_, state: &mut PipelineState| {
                let mut approved = Vec::new();
                for pilot in pilots {
                    let open = state
                        .child_dispatches
                        .iter()
                        .any(|child| child.child_run_id == pilot.run_id && child.phase.is_open());
                    if open {
                        approved.extend(pilot.approved.iter().cloned());
                        state.advance_child_dispatch(
                            &pilot.run_id,
                            ChildDispatchPhase::Terminal,
                            Some(pilot.status.to_string()),
                            None,
                        );
                    }
                }
                // A reused assessment is approved once, by the pass whose
                // promotion moved it, so only this pass's approvals can
                // repeat it.
                for task_id in reused {
                    if !approved.contains(task_id) {
                        approved.push(task_id.clone());
                    }
                }
                let report = state
                    .pull_drain
                    .drain_approvals
                    .get_or_insert_with(DrainApprovalReport::default);
                report.recorded_at = Some(Utc::now());
                report.approved_total += approved.len() as u64;
                report.approved.extend(approved);
                let excess = report.approved.len().saturating_sub(REPORTED_TASKS);
                report.approved.drain(..excess);
                report.closed_total += closed.len() as u64;
                report.closed.extend(closed.iter().cloned());
                let excess = report.closed.len().saturating_sub(REPORTED_TASKS);
                report.closed.drain(..excess);
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

/// Record every finished pilot of this drain not recorded yet: which of the
/// tasks in its own run input this drain approved, and the refreshed held
/// view. The pass that sees a pilot finish runs this before admission, so
/// the tasks it approved are admitted in that same pass.
pub(crate) fn record_proposed_approvals(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let run_id = drain_run_id(input).ok_or_else(|| {
        DispatchError::action_failed(action, "the drain run id is unavailable".to_string())
    })?;
    let pilots = read_pilots(runtime, action, run_id)?;
    record_unresolved_pilots(runtime, &pilots);
    let recorded = pilots
        .iter()
        .filter(|pilot| pilot.awaits_recording())
        .filter_map(|pilot| {
            Some(RecordedPilot {
                run_id: pilot.run_id.clone(),
                status: pilot.finished?,
                approved: approved_by(runtime, run_id, &pilot.task_ids),
            })
        })
        .collect::<Vec<_>>();
    let admissions_stopped = live_admissions_stop(runtime, input).is_some();
    let snapshot = proposed_tasks(runtime)
        .and_then(|proposed| approval_snapshot(runtime, proposed, &pilots, !admissions_stopped))
        .map_err(|error| {
            DispatchError::action_failed(action, format!("read proposed tasks: {error}"))
        })?;
    record_report(runtime, run_id, &snapshot, &recorded, &[]);
    let approved = recorded
        .iter()
        .flat_map(|pilot| pilot.approved.iter().cloned())
        .collect::<Vec<_>>();
    let mut output = held_json(&snapshot);
    output["approved_count"] = json!(approved.len());
    output["approved"] = json!(approved);
    output["pilots"] = recorded
        .iter()
        .map(|pilot| {
            json!({
                "run_id": pilot.run_id,
                "status": pilot.status.to_string(),
                "approved": pilot.approved,
            })
        })
        .collect();
    Ok(output)
}

/// Block until no task-pilot child of this drain is live, or until
/// `timeout_seconds` passes. Inside the loop it bounds the wait by the poll
/// interval, so admission still runs that often; after the loop it keeps the
/// drain alive until its last pilot finishes, because the pilot's apply step
/// refuses the authority of a drain that has ended.
pub(crate) fn await_approval_pilot(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let run_id = drain_run_id(input).ok_or_else(|| {
        DispatchError::action_failed(action, "the drain run id is unavailable".to_string())
    })?;
    let timeout_seconds = templated_u64(action, input, "timeout_seconds", 0)?;
    if timeout_seconds > MAX_PILOT_WAIT_SECONDS {
        return Err(DispatchError::action_failed(
            action,
            format!("`timeout_seconds` must be at most {MAX_PILOT_WAIT_SECONDS}"),
        ));
    }
    let timeout = Duration::from_secs(timeout_seconds);
    let started_at = Instant::now();
    loop {
        let live = read_pilots(runtime, action, run_id)?
            .into_iter()
            .filter(PilotChild::is_live)
            .map(|pilot| pilot.run_id)
            .collect::<Vec<_>>();
        let waited = started_at.elapsed();
        if live.is_empty() || waited >= timeout {
            return Ok(json!({
                "pilot_live": !live.is_empty(),
                "live_pilot_run_ids": live,
                "waited_seconds": waited.as_secs_f64(),
            }));
        }
        std::thread::sleep(PILOT_WAIT_POLL.min(timeout - waited));
    }
}
