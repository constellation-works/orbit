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
//! Selection decides who is worth a pilot. A proposed task whose latest
//! certified assessment came from an explicit or routine pilot, and is still
//! fresh, is approved here without another provider call. A drain child that
//! stood down cannot claim that receipt as an unfinished approval. A stale
//! one (its description or criteria changed after the assessment) is selected
//! again for a fresh pilot like a new proposal, unless a live execution claim
//! holds it; one with a holding warning stays proposed and is not selected again.
//! Every other approval happens in the task-pilot apply boundary under the
//! drain's verified authority, so the qualification checked here is a cost
//! filter, not the gate. Targeted task ids and promotion authority are not a
//! new pilot request. A task the pilot held is not piloted again until it
//! changes: the pilot entry that records the hold stays the newest in its
//! history. A pilot that finished
//! without deciding about a task (a failed or stale pilot) is recorded on that
//! task when the drain that ran it records its approvals, so no later drain
//! pilots the task again until it changes either. The same recording covers
//! an applied assessment whose approval did not complete: it stays
//! `pilot_unresolved` until the task changes or an operator approves it. A
//! fresh explicit or routine assessment is not that failure. This drain
//! promotes it, and a finished child that stood down after the explicit
//! apply does not hold or record the task as unresolved. A pilot that failed on a
//! provider outage or missing inspection-capable crew is not recorded, so the
//! next drain retries it. Creation-grant rows are not changes here: every write
//! to a task holding a grant appends one after its own entry to re-seal it.
//!
//! A task whose current pilot assessment is `verified_no_diff` is held as
//! `pilot_verified_no_diff` with the pilot's evidence. An approval pass
//! archives one Orbit automation filed once its cited commits are proven on
//! the base branch; the read-only readiness view only reports it.

use std::collections::BTreeMap;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::task::{NO_AUTO_APPROVE_TAG, NO_DRAIN_TAG, Task, TaskStatus};
use orbit_types::workflow::{DrainHoldFinding, DrainWaitingTask};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::{
    NoDiffClosure, PILOT_VERIFIED_NO_DIFF, TaskAutomationPort, TaskListFilter, metadata_task,
};
use crate::runtime::engine::crew::normalized_task_crew;

use super::drains::{drain_run_id, live_admissions_stop};
use crate::application::task::pilot::{
    ExplicitAssessmentReuse, FOOTPRINT_UNRESOLVED, approval_disqualification,
    classify_explicit_assessment_reuse, held_classification, live_execution_claim_tasks,
    promote_reusable_assessment, task_pilot_warnings,
};

/// Most proposed tasks one pass pilots; the rest wait for the next pass.
const MAX_CANDIDATES_PER_PASS: usize = 10;
/// History event the drain records, by `system`, on a proposed task whose
/// task-pilot run finished without deciding about it. Its note names the run.
/// Later drains hold the task on it until a real change, whichever drain
/// dispatched that pilot [ORB-19544].
pub(super) const UNRESOLVED_PILOT_EVENT: &str = "task_pilot_unresolved";
/// Bounded lists on the durable report; totals sit beside them.
pub(super) const REPORTED_TASKS: usize = 20;
pub(super) const PILOT_JOB: &str = "task_pilot_pipeline";
/// How often a wait re-reads whether the drain's pilot is still live.
pub(super) const PILOT_WAIT_POLL: Duration = Duration::from_secs(1);
/// Longest a single wait may block, matching the `sleep` activity's bound.
pub(super) const MAX_PILOT_WAIT_SECONDS: u64 = 3600;

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
    /// Fresh explicit or routine assessments this pass can approve without
    /// another pilot. Not candidates: a drain child must not assess them again.
    pub(super) reusable: Vec<String>,
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

/// The workspace's `proposed` tasks, as envelope metadata.
pub(super) fn proposed_tasks(runtime: &OrbitRuntime) -> Result<Vec<Task>, OrbitError> {
    Ok(runtime
        .task_candidates(
            &TaskListFilter {
                statuses: Some(vec![TaskStatus::Proposed]),
                ..TaskListFilter::default()
            },
            usize::MAX,
        )?
        .items
        .into_iter()
        .map(metadata_task)
        .collect())
}

/// Classify the workspace's `proposed` tasks against the drain's task-pilot
/// children `pilots`. With `close`, a proven `verified_no_diff` task Orbit
/// automation filed is archived instead of held.
pub(super) fn approval_snapshot(
    runtime: &OrbitRuntime,
    mut proposed: Vec<Task>,
    pilots: &[PilotChild],
    close: bool,
) -> Result<ApprovalSnapshot, OrbitError> {
    proposed.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then(left.id.cmp(&right.id))
    });
    let mut snapshot = ApprovalSnapshot {
        candidates: Vec::new(),
        held: Vec::new(),
        closed: Vec::new(),
        reusable: Vec::new(),
    };
    let holders = runtime.pilot_preparation_holders()?;
    // A stale assessment on a claimed task keeps holding: the claim is still
    // executing against the meaning the claim froze. Everywhere else a stale
    // assessment is a repilot candidate below, not an operator hold.
    let claimed_tasks = live_execution_claim_tasks(runtime)?;
    for task in proposed {
        let held = |reason: &str, detail: Option<String>, findings: Vec<DrainHoldFinding>| {
            DrainWaitingTask {
                task_id: task.id.clone(),
                reason: Some(reason.to_string()),
                blocked_by: Vec::new(),
                detail,
                findings,
            }
        };
        let disqualified =
            approval_disqualification(&task.tags, &task.context_files, task.complexity);
        if disqualified != Some(NO_AUTO_APPROVE_TAG)
            && disqualified != Some(NO_DRAIN_TAG)
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
                .push(held(PILOT_VERIFIED_NO_DIFF, Some(detail), Vec::new()));
            continue;
        }
        if let Some(reason) = disqualified {
            snapshot.held.push(held(reason, None, Vec::new()));
            continue;
        }
        let history = runtime.get_task_history(&task.id)?;
        let latest = history
            .iter()
            .rev()
            .find(|entry| !is_decision_neutral(entry));
        if let Some(entry) = latest.filter(|entry| entry.event == "task_pilot_applied")
            && let Some(classification) = held_classification(runtime, entry)
            && !(classification == "changed_since_pilot" && !claimed_tasks.contains(&task.id))
        {
            let findings = if matches!(classification, "warnings" | FOOTPRINT_UNRESOLVED) {
                task_pilot_warnings(
                    task.status,
                    &runtime.get_task_comments(&task.id)?,
                    &task.dependencies(),
                    &|id: &str| runtime.warning_prerequisite_is_done(id),
                )
            } else {
                Vec::new()
            };
            // A typed warnings hold whose findings no longer hold (a
            // prerequisite became a dependency edge, or finished) releases
            // the task to be piloted and promoted by this pass without an
            // operator edit. Legacy note-derived holds have no receipt to
            // re-evaluate, so they stay held.
            let typed_warnings = matches!(
                &entry.decision,
                Some(orbit_types::task::TaskDecision::DrainApprovalHeld { classification })
                    if classification == "warnings"
            );
            if typed_warnings && findings.is_empty() {
                snapshot.candidates.push(ApprovalCandidate {
                    id: task.id,
                    crew: normalized_task_crew(task.crew.as_deref()),
                });
                continue;
            }
            snapshot.held.push(held(classification, None, findings));
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
                Vec::new(),
            ));
            continue;
        }
        // Nor is one an explicit or routine pilot is running: the child this
        // pass would dispatch could only stand down.
        if let Some(run_id) = live_pilot_run(runtime, &holders, &task.id)? {
            snapshot.held.push(held(
                "pilot_running",
                Some(format!("task-pilot run {run_id} is assessing it")),
                Vec::new(),
            ));
            continue;
        }
        // A certified explicit receipt owns its assessment. A drain child
        // may have stood down while that receipt was being applied, so its
        // submission time alone cannot make the assessment unresolved.
        match classify_explicit_assessment_reuse(runtime, &task)? {
            ExplicitAssessmentReuse::Promote { .. } => {
                snapshot.reusable.push(task.id);
                continue;
            }
            ExplicitAssessmentReuse::Hold { reason, detail } => {
                // The task's meaning changed after its certified assessment
                // and no claim holds it: queue a fresh pilot like a new
                // proposal instead of holding for an operator repilot. The
                // unresolved-pilot checks below still hold it when that
                // repilot already failed.
                if reason == "changed_since_pilot" && !claimed_tasks.contains(&task.id) {
                    // Fall through to the unresolved checks and candidacy.
                } else {
                    let findings = if matches!(reason, "warnings" | FOOTPRINT_UNRESOLVED) {
                        task_pilot_warnings(
                            task.status,
                            &runtime.get_task_comments(&task.id)?,
                            &task.dependencies(),
                            &|id: &str| runtime.warning_prerequisite_is_done(id),
                        )
                    } else {
                        Vec::new()
                    };
                    snapshot.held.push(held(reason, detail, findings));
                    continue;
                }
            }
            ExplicitAssessmentReuse::NotReusable => {}
        }
        // An applied assessment with no hold whose task is still proposed
        // was promoted on paper but its approval did not complete (for
        // example a deterministic apply failure after the pilot write).
        // Hold it for the operator instead of piloting it again on every
        // drain pass [ORB-21172]. A fresh explicit or routine assessment
        // is this drain's to promote: a finished child that stood down
        // after that apply must not hide the reuse [ORB-21206]. A drain
        // child's own unapproved apply is not reusable, so it stays held.
        if let Some(entry) = latest.filter(|entry| entry.event == "task_pilot_applied")
            && held_classification(runtime, entry).is_none()
            && let Some(pilot) = pilots.iter().find(|pilot| {
                !pilot.is_live()
                    && pilot.task_ids.contains(&task.id)
                    && entry.at >= pilot.submitted_at
            })
            && !reusable_explicit_promotion(runtime, &task)?
        {
            snapshot.held.push(held(
                "pilot_unresolved",
                Some(format!(
                    "task-pilot run {} applied its assessment but approval did not complete; \
                     edit the task or approve it to retry",
                    pilot.run_id
                )),
                Vec::new(),
            ));
            continue;
        }
        let changed_at = latest.map(|entry| entry.at);
        // An undecided pilot an earlier drain recorded holds the task until a
        // real change, even though no pilot of this drain ran it.
        if let Some(entry) = history
            .iter()
            .rev()
            .find(|entry| entry.event == UNRESOLVED_PILOT_EVENT)
            && changed_at.is_none_or(|at| entry.at > at)
            && !unresolved_pilot_failed_independently(runtime, entry)?
        {
            snapshot.held.push(held(
                "pilot_unresolved",
                Some(format!(
                    "task-pilot run {} left no decision; edit the task or approve it to retry",
                    unresolved_run_id(entry).unwrap_or_else(|| "unknown".to_string())
                )),
                Vec::new(),
            ));
            continue;
        }
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
                Vec::new(),
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

pub(super) fn held_json(snapshot: &ApprovalSnapshot) -> Value {
    json!({
        "closed": snapshot.closed,
        "held": snapshot.held.iter().take(REPORTED_TASKS).collect::<Vec<_>>(),
        "held_total": snapshot.held.len(),
        "held_by_reason": snapshot.held_by_reason(),
        "awaiting_reuse": snapshot.reusable.iter().take(REPORTED_TASKS).collect::<Vec<_>>(),
        "awaiting_reuse_total": snapshot.reusable.len(),
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
pub(crate) fn select_proposed_approvals(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let enabled = match input.get("approve_proposed") {
        None | Some(Value::Null) => false,
        Some(value) => value.as_bool().ok_or_else(|| {
            DispatchError::action_failed(action, "approve_proposed must be a boolean".to_string())
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
    let mut snapshot = proposed_tasks(runtime)
        .and_then(|proposed| approval_snapshot(runtime, proposed, &pilots, !admissions_stopped))
        .map_err(|error| {
            DispatchError::action_failed(action, format!("read proposed tasks: {error}"))
        })?;
    let mut reused = Vec::new();
    // While this drain's own pilot is live, leave a certified assessment
    // proposed. The next pass approves it once that pilot has finished.
    if !admissions_stopped && !pilot_live && !snapshot.reusable.is_empty() {
        for task_id in &snapshot.reusable {
            if promote_reusable_assessment(runtime, run_id, task_id).map_err(|error| {
                DispatchError::action_failed(
                    action,
                    format!("reuse assessment for {task_id}: {error}"),
                )
            })? {
                reused.push(task_id.clone());
            }
        }
        snapshot = proposed_tasks(runtime)
            .and_then(|proposed| approval_snapshot(runtime, proposed, &pilots, !admissions_stopped))
            .map_err(|error| {
                DispatchError::action_failed(action, format!("read proposed tasks: {error}"))
            })?;
    }
    record_report(runtime, run_id, &snapshot, &[], &reused);
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
    output["reused"] = json!(reused);
    output["reused_count"] = json!(reused.len());
    output["drain_promotion"] = json!({ "run_id": run_id });
    Ok(output)
}

/// Readiness's view of `--approve-proposed` for the status drain: what it
/// has approved so far and why each remaining proposed task is held. A drain
/// started without the flag reports only `enabled: false`.
///
/// `population` is the readiness snapshot's task lookup. It already holds
/// every open task, `proposed` included, so the held view classifies those
/// rather than paying a second workspace-wide index freshness scan for the
/// same tasks.
pub(super) fn readiness_approvals(
    runtime: &OrbitRuntime,
    drain_run_id: Option<&str>,
    drain_input: Option<&Value>,
    population: &BTreeMap<String, Task>,
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
        .and_then(|state| state.pull_drain.drain_approvals)
        .unwrap_or_default();
    // A replica sees no proposed tasks here, as `task_candidates` would
    // return none to it.
    let proposed = if runtime.coordination_task_reads_visible() {
        population
            .values()
            .filter(|task| task.status == TaskStatus::Proposed)
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    let snapshot = approval_snapshot(runtime, proposed, &pilot_children(runtime, run_id)?, false)?;
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

use super::pilot_coordination::{
    PilotChild, is_decision_neutral, live_pilot_run, pilot_children, read_pilots, record_report,
    reusable_explicit_promotion, unresolved_pilot_failed_independently, unresolved_run_id,
};
pub(crate) use super::pilot_coordination::{await_approval_pilot, record_proposed_approvals};
