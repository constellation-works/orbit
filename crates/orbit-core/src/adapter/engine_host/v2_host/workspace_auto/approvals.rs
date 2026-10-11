//! `orbit run auto --approve-proposed`: which `proposed` tasks each drain
//! pass pilots for approval, the wait on that pilot, and the durable report
//! of what it approved and held.
//!
//! The pilot runs detached, one at a time, so admission never waits on it. A
//! pass starts one only when no pilot of this drain is live, and the pass
//! that sees it finished records its approvals before admitting. A task a
//! live pilot carries is held as `pilot_running` and never selected again
//! while that pilot runs.
//!
//! Selection only decides who is worth a pilot. Approval itself happens in
//! the task-pilot apply boundary under the drain's verified authority, so the
//! qualification checked here is a cost filter, not the gate. A task the
//! pilot held is not piloted again until it changes: its hold marker stays
//! the newest entry in its history, and a pilot this drain already ran that
//! left no decision behind (a failed or stale pilot) is not retried until the
//! task changes either. Creation-grant rows are not changes here: every write
//! to a task holding a grant appends one after its own entry to re-seal it.
//!
//! A task whose current pilot assessment is `verified_no_diff` is held as
//! `pilot_verified_no_diff` with the pilot's evidence. An approval pass
//! archives one Orbit automation filed once its cited commits are proven on
//! the base branch; the read-only readiness view only reports it.
//!
//! A review-filed bug without a CONFIRMED verification verdict is held as
//! `unverified_review_finding` before it costs a pilot: approval would refuse
//! it anyway, and the detail names the verification job.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::task::{
    CONTEXT_CREATION_AUTHORIZED_EVENT, NO_AUTO_APPROVE_TAG, TaskStatus, TaskType,
};
use orbit_types::workflow::{
    ChildDispatchPhase, DrainApprovalReport, DrainWaitingTask, JobRunState, PipelineState,
};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::{
    NoDiffClosure, PILOT_VERIFIED_NO_DIFF, TaskListFilter, UNVERIFIED_REVIEW_FINDING,
};
use crate::runtime::engine::crew::normalized_task_crew;

use super::super::task_pilot::{approval_disqualification, approved_by_drain, held_classification};
use super::action_failed;
use super::classify::templated_u64;
use super::drains::live_admissions_stop;

/// Most proposed tasks one pass pilots; the rest wait for the next pass.
const MAX_CANDIDATES_PER_PASS: usize = 10;
/// Bounded lists on the durable report; totals sit beside them.
const REPORTED_TASKS: usize = 20;
const PILOT_JOB: &str = "task_pilot_pipeline";
/// How often a wait re-reads whether the drain's pilot is still live.
const PILOT_WAIT_POLL: Duration = Duration::from_secs(1);
/// Longest a single wait may block, matching the `sleep` activity's bound.
const MAX_PILOT_WAIT_SECONDS: u64 = 3600;

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
    /// Tasks this pass archived as proven already fixed.
    pub(super) closed: Vec<String>,
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

/// Classify the workspace's `proposed` tasks against the drain's task-pilot
/// children `pilots`. With `close`, a proven `verified_no_diff` task Orbit
/// automation filed is archived instead of held.
fn approval_snapshot(
    runtime: &OrbitRuntime,
    pilots: &[PilotChild],
    close: bool,
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
    let mut snapshot = ApprovalSnapshot {
        candidates: Vec::new(),
        held: Vec::new(),
        closed: Vec::new(),
    };
    for task in proposed {
        let held = |reason: &str, detail: Option<String>| DrainWaitingTask {
            task_id: task.id.clone(),
            reason: Some(reason.to_string()),
            blocked_by: Vec::new(),
            detail,
        };
        let disqualified =
            approval_disqualification(&task.tags, &task.context_files, task.complexity);
        if disqualified != Some(NO_AUTO_APPROVE_TAG)
            && let Some((_, finding)) =
                runtime.verified_no_diff(&task.id, task.status, &task.tags)?
        {
            // The close re-reads the finding under the task lock. A failed
            // close leaves the task held rather than failing the pass.
            let detail = match close.then(|| runtime.close_verified_no_diff(&task.id)) {
                Some(Ok(Some(NoDiffClosure::Archived { .. }))) => {
                    snapshot.closed.push(task.id.clone());
                    continue;
                }
                Some(Ok(Some(NoDiffClosure::Held { finding, reason }))) => {
                    format!("{}; not closed: {reason}", finding.detail())
                }
                Some(Err(error)) => {
                    tracing::warn!(
                        target: "orbit.core.job_run",
                        task_id = %task.id,
                        %error,
                        "drain could not close a task task-pilot verified as already fixed"
                    );
                    format!("{}; not closed: {error}", finding.detail())
                }
                Some(Ok(None)) | None => finding.detail(),
            };
            snapshot
                .held
                .push(held(PILOT_VERIFIED_NO_DIFF, Some(detail)));
            continue;
        }
        if let Some(reason) = disqualified {
            snapshot.held.push(held(reason, None));
            continue;
        }
        if task.task_type == TaskType::Bug
            && let Some(hold) = runtime.unverified_finding_hold(&runtime.get_task(&task.id)?)?
        {
            snapshot
                .held
                .push(held(UNVERIFIED_REVIEW_FINDING, Some(hold.detail())));
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
        // A pilot still running would be duplicated by a second one.
        if let Some(pilot) = pilots
            .iter()
            .find(|pilot| pilot.is_live() && pilot.task_ids.contains(&task.id))
        {
            snapshot.held.push(held(
                "pilot_running",
                Some(format!("task-pilot run {} is assessing it", pilot.run_id)),
            ));
            continue;
        }
        let changed_at = latest.map(|entry| entry.at);
        if let Some(run_id) = pilots.iter().find_map(|pilot| {
            (!pilot.is_live()
                && pilot.task_ids.contains(&task.id)
                && changed_at.is_none_or(|at| pilot.submitted_at > at))
            .then_some(pilot.run_id.as_str())
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

/// One task-pilot child this drain dispatched.
struct PilotChild {
    run_id: String,
    /// The tasks it was asked to pilot, from its own run input.
    task_ids: Vec<String>,
    submitted_at: DateTime<Utc>,
    /// Its terminal state; `None` while it is pending or running.
    finished: Option<JobRunState>,
    /// Whether the drain still has to record what it approved. Recording
    /// closes the dispatch, so each pilot is counted once.
    unrecorded: bool,
}

impl PilotChild {
    fn is_live(&self) -> bool {
        self.finished.is_none()
    }

    fn awaits_recording(&self) -> bool {
        self.unrecorded && !self.is_live()
    }
}

/// The task-pilot children this drain dispatched, live and finished.
fn pilot_children(runtime: &OrbitRuntime, run_id: &str) -> Result<Vec<PilotChild>, OrbitError> {
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
        pilots.push(PilotChild {
            run_id: run.run_id,
            task_ids,
            submitted_at: child.submitted_at,
            finished: run.state.is_terminal().then_some(run.state),
            unrecorded: child.phase.is_open(),
        });
    }
    Ok(pilots)
}

fn read_pilots(
    runtime: &OrbitRuntime,
    action: &str,
    run_id: &str,
) -> Result<Vec<PilotChild>, DispatchError> {
    pilot_children(runtime, run_id)
        .map_err(|error| action_failed(action, format!("read task-pilot children: {error}")))
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

/// A finished pilot and the tasks this drain approved through it.
struct RecordedPilot {
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
fn record_report(
    runtime: &OrbitRuntime,
    run_id: &str,
    snapshot: &ApprovalSnapshot,
    pilots: &[RecordedPilot],
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
                let report = state
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

fn drain_run_id(input: &Value) -> Option<&str> {
    input
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn held_json(snapshot: &ApprovalSnapshot) -> Value {
    json!({
        "closed": snapshot.closed,
        "held": snapshot.held.iter().take(REPORTED_TASKS).collect::<Vec<_>>(),
        "held_total": snapshot.held.len(),
        "held_by_reason": snapshot.held_by_reason(),
    })
}

/// One pass's selection: the qualifying `proposed` tasks to pilot now, the
/// drain-scoped authority record the pilot's apply step verifies, and the
/// state of this drain's pilot.
///
/// Nothing is selected while a pilot of this drain is live, so at most one
/// runs at a time. `pilot_active` (a pilot is live, or this pass starts one)
/// and `pilot_finished` (a finished pilot awaits recording) are always
/// defined, so the loop can gate its waits on them even when the record step
/// is skipped.
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
            "pilot_active": false,
            "pilot_live": false,
            "live_pilot_run_ids": [],
            "pilot_finished": false,
        }));
    };
    let pilots = read_pilots(runtime, action, run_id)?;
    let live_pilot_run_ids = pilots
        .iter()
        .filter(|pilot| pilot.is_live())
        .map(|pilot| pilot.run_id.clone())
        .collect::<Vec<_>>();
    let pilot_live = !live_pilot_run_ids.is_empty();
    // A stopped drain admits nothing, so it approves and closes nothing either.
    let admissions_stopped = live_admissions_stop(runtime, input).is_some();
    let snapshot = approval_snapshot(runtime, &pilots, !admissions_stopped)
        .map_err(|error| action_failed(action, format!("read proposed tasks: {error}")))?;
    record_report(runtime, run_id, &snapshot, &[]);
    let task_ids = if admissions_stopped || pilot_live {
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
    output["pilot_active"] = json!(pilot_live || !task_ids.is_empty());
    output["pilot_live"] = json!(pilot_live);
    output["live_pilot_run_ids"] = json!(live_pilot_run_ids);
    output["pilot_finished"] = json!(pilots.iter().any(PilotChild::awaits_recording));
    output["task_ids"] = json!(task_ids);
    output["drain_promotion"] = json!({ "run_id": run_id });
    Ok(output)
}

/// Record every finished pilot of this drain not recorded yet: which of the
/// tasks in its own run input this drain approved, and the refreshed held
/// view. The pass that sees a pilot finish runs this before admission, so
/// the tasks it approved are admitted in that same pass.
pub(in super::super) fn record_proposed_approvals(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let run_id = drain_run_id(input)
        .ok_or_else(|| action_failed(action, "the drain run id is unavailable".to_string()))?;
    let pilots = read_pilots(runtime, action, run_id)?;
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
    let snapshot = approval_snapshot(runtime, &pilots, !admissions_stopped)
        .map_err(|error| action_failed(action, format!("read proposed tasks: {error}")))?;
    record_report(runtime, run_id, &snapshot, &recorded);
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
pub(in super::super) fn await_approval_pilot(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let run_id = drain_run_id(input)
        .ok_or_else(|| action_failed(action, "the drain run id is unavailable".to_string()))?;
    let timeout_seconds = templated_u64(action, input, "timeout_seconds", 0)?;
    if timeout_seconds > MAX_PILOT_WAIT_SECONDS {
        return Err(action_failed(
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
    let snapshot = approval_snapshot(runtime, &pilot_children(runtime, run_id)?, false)?;
    let mut value = held_json(&snapshot);
    value["closed"] = json!(report.closed);
    value["closed_total"] = json!(report.closed_total);
    value["enabled"] = json!(true);
    value["drain_run_id"] = json!(run_id);
    value["approved_total"] = json!(report.approved_total);
    value["approved"] = json!(report.approved);
    value["awaiting_pilot"] = json!(snapshot.candidates.len());
    Ok(value)
}
