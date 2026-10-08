//! Drain-scoped promotion authority for `orbit run auto --approve-proposed`.
//!
//! A drain started with `approve_proposed` pilots the qualifying `proposed`
//! tasks it selected and hands this boundary its own run ID as the authority
//! record. The record is verified rather than trusted: the named run must be
//! a live auto drain started with the flag, and it must have dispatched the
//! very pilot run applying the results. A task that then passes the shared
//! promotion findings is approved through the ordinary approve transition,
//! with a note naming the drain; any other task stays `proposed` and carries a
//! hold marker in its pilot history entry, so the next pass does not pilot it
//! again until the task changes. The approval rechecks the opt-out tag and the
//! pilot-assessed material under the task lock it writes under, so an operator
//! edit racing the drain is never approved over.

use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::task::{
    NO_AUTO_APPROVE_TAG, NO_DIFF_EXPECTED_TAG, ReadinessGap, ReadinessStage, Task, TaskComplexity,
    TaskStatus, readiness_gaps,
};
use orbit_types::workflow::automation::members::PreparationPolicy;
use serde_json::{Value, json};

use crate::OrbitRuntime;

use super::apply::{PreparedTaskSnapshot, ValidatedTask};
use super::input::action_failed;
use super::persist::{assessed_material_drift, with_task_locks};
use super::promotion::{PromotionFindings, auto_approval_opted_out, promotion_findings};

const DRAIN_JOB: &str = "workspace_auto_pipeline";
const PILOT_JOB: &str = "task_pilot_pipeline";
const HELD_MARKER_PREFIX: &str = " [drain-approval-held:";

/// Why a `proposed` task does not qualify for drain approval, before any
/// pilot runs: a `no-auto-approve` tag excludes it outright; otherwise its
/// first blocking readiness gap ([`readiness_gaps`]) withholds it.
pub(in crate::adapter::engine_host::v2_host) fn approval_disqualification(
    tags: &[String],
    context_files: &[String],
    complexity: Option<TaskComplexity>,
) -> Option<&'static str> {
    if auto_approval_opted_out(tags) {
        return Some(NO_AUTO_APPROVE_TAG);
    }
    readiness_gaps(ReadinessStage::Proposed, tags, context_files, complexity)
        .into_iter()
        .find(ReadinessGap::is_blocking)
        .map(|gap| gap.code.as_str())
}

/// The approve-transition note; task history names the approving drain.
pub(in crate::adapter::engine_host::v2_host) fn approval_note(run_id: &str) -> String {
    format!("approved by auto drain {run_id} after task-pilot verification")
}

/// Whether a `proposal_approved` history note records this drain's approval.
pub(in crate::adapter::engine_host::v2_host) fn approved_by_drain(
    note: &str,
    run_id: &str,
) -> bool {
    note.starts_with(&approval_note(run_id))
}

fn held_marker(classification: &str) -> String {
    format!("{HELD_MARKER_PREFIX}{classification}]")
}

/// The pilot classification a drain hold recorded in a `task_pilot_applied`
/// history note.
pub(in crate::adapter::engine_host::v2_host) fn held_classification(note: &str) -> Option<&str> {
    let (_, marker) = note.split_once(HELD_MARKER_PREFIX)?;
    marker
        .split_once(']')
        .map(|(classification, _)| classification)
}

/// A verified drain authority record.
pub(super) struct DrainAuthority {
    pub(super) run_id: String,
}

impl DrainAuthority {
    /// Verify `record` (`{"run_id": ...}`) against the stores. `pilot_run_id`
    /// is the engine-injected ID of the run applying these results, never a
    /// value the caller chose.
    pub(super) fn verify(
        runtime: &OrbitRuntime,
        action: &str,
        record: &Value,
        pilot_run_id: Option<&str>,
    ) -> Result<Self, DispatchError> {
        let run_id = record
            .get("run_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|run_id| !run_id.is_empty())
            .ok_or_else(|| action_failed(action, "drain_promotion.run_id must be a string"))?;
        let refused = |reason: &str| {
            action_failed(
                action,
                format!("drain promotion authority from run {run_id} refused: {reason}"),
            )
        };
        let drain = runtime
            .stores()
            .jobs()
            .get_job_run(run_id)
            .map_err(|error| action_failed(action, error.to_string()))?
            .ok_or_else(|| refused("no such run"))?;
        if drain.job_id != DRAIN_JOB {
            return Err(refused("it is not an auto drain"));
        }
        if drain.state.is_terminal() {
            return Err(refused("the drain has ended"));
        }
        if drain
            .input
            .as_ref()
            .and_then(|input| input.get("approve_proposed"))
            .and_then(Value::as_bool)
            != Some(true)
        {
            return Err(refused("the drain was not started with approve_proposed"));
        }
        let pilot_run_id = pilot_run_id.ok_or_else(|| refused("the pilot run is unknown"))?;
        let dispatched = runtime
            .stores()
            .jobs()
            .read_run_state(run_id)
            .map_err(|error| action_failed(action, error.to_string()))?
            .is_some_and(|state| {
                state
                    .child_dispatches
                    .iter()
                    .any(|child| child.child_run_id == pilot_run_id && child.job_name == PILOT_JOB)
            });
        if !dispatched {
            return Err(refused("the drain did not dispatch this pilot run"));
        }
        Ok(Self {
            run_id: run_id.to_string(),
        })
    }
}

/// Decide one task under drain authority. Pilot output that is malformed
/// fails the task; every well-formed finding becomes a classified hold.
#[allow(clippy::too_many_arguments)]
pub(super) fn assess(
    action: &str,
    task_id: &str,
    snapshot: &PreparedTaskSnapshot,
    current: &Task,
    assessment: &Value,
    selectors: &[String],
    complexity: TaskComplexity,
    authority: &DrainAuthority,
) -> Result<Value, DispatchError> {
    let disposition = assessment
        .get("disposition")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let PromotionFindings {
        duplicate_of,
        already_landed,
        warnings,
    } = promotion_findings(action, task_id, assessment)?;
    let release_action = assessment
        .get("release_action_required")
        .filter(|finding| !finding.is_null());
    let no_diff = snapshot.tags.iter().any(|tag| tag == NO_DIFF_EXPECTED_TAG);

    let (decision, classification, evidence) = if current.status != TaskStatus::Proposed {
        (
            "withhold",
            "not_proposed",
            json!(format!("task is {}", current.status)),
        )
    } else if auto_approval_opted_out(&current.tags) {
        ("withhold", NO_AUTO_APPROVE_TAG, Value::Null)
    } else if let Some(reason) =
        approval_disqualification(&snapshot.tags, &snapshot.context_files, snapshot.complexity)
    {
        ("withhold", reason, Value::Null)
    } else if !already_landed.is_null() {
        ("withhold", "already_landed", already_landed.clone())
    } else if !duplicate_of.is_null() {
        ("withhold", "duplicate", duplicate_of.clone())
    } else if let Some(finding) = release_action {
        ("withhold", "release_action_required", finding.clone())
    } else if !warnings.is_empty() {
        ("withhold", "warnings", json!(warnings))
    } else if !no_diff && (disposition != "selectors" || selectors.is_empty()) {
        (
            "withhold",
            "no_actionable_selectors",
            assessment.get("evidence").cloned().unwrap_or(Value::Null),
        )
    } else if !no_diff && !complexity.is_assessed() {
        ("withhold", "unassessed_complexity", Value::Null)
    } else {
        (
            "promote",
            "verified_ready",
            json!(
                "pilot validated the task with no duplicate, already-landed, conflict, or warning finding"
            ),
        )
    };
    Ok(json!({
        "task_id": task_id,
        "decision": decision,
        "classification": classification,
        "drain_run_id": authority.run_id,
        "evidence": evidence,
    }))
}

/// The history marker a hold leaves on the pilot's own history entry.
pub(super) fn hold_marker(admission: &Value) -> Option<String> {
    (admission["decision"] == "withhold")
        .then(|| admission["classification"].as_str().map(held_marker))
        .flatten()
}

/// What the drain's approve step did with a promoted task.
pub(super) enum Approval {
    Approved,
    /// The task already left `proposed` (a replayed apply, or an operator who
    /// got there first), so it is not approved twice.
    NotProposed,
    /// Approval was withheld after the pilot wrote its assessment.
    Held {
        classification: &'static str,
        evidence: Value,
    },
}

/// Approve a promoted task through the ordinary approve transition. The
/// decision is made under the task and dependency locks the pilot write took,
/// and the transition runs inside them, so a concurrent edit either lands
/// before the check or waits behind the approval. A task tagged
/// `no-auto-approve` since it was assessed is held, as is one whose
/// pilot-assessed material changed after the pilot write: the next drain pass
/// pilots it again.
pub(super) fn approve(
    runtime: &OrbitRuntime,
    validated: &ValidatedTask,
    snapshot: &PreparedTaskSnapshot,
    policy: &PreparationPolicy,
    authority_run_id: &str,
) -> Result<Approval, OrbitError> {
    let task_id = validated.task_id.as_str();
    let mut lock_ids = vec![task_id.to_string()];
    lock_ids.extend(runtime.get_task(task_id)?.dependencies());
    lock_ids.sort();
    lock_ids.dedup();
    #[cfg(test)]
    approval_hook::before_lock(runtime, task_id);
    let mut approval = None;
    with_task_locks(runtime, &lock_ids, 0, &mut || {
        let task = runtime.get_task(task_id)?;
        approval = Some(if task.status != TaskStatus::Proposed {
            Approval::NotProposed
        } else if auto_approval_opted_out(&task.tags) {
            Approval::Held {
                classification: NO_AUTO_APPROVE_TAG,
                evidence: Value::Null,
            }
        } else if let Some((reason, detail)) = assessed_material_drift(
            runtime,
            &task,
            snapshot,
            &validated.after,
            validated.complexity,
            policy,
        ) {
            Approval::Held {
                classification: "changed_since_pilot",
                evidence: json!({ "reason": reason, "detail": detail }),
            }
        } else {
            runtime.approve_task(task_id, Some(approval_note(authority_run_id)), None)?;
            Approval::Approved
        });
        Ok(())
    })?;
    approval.ok_or_else(|| {
        OrbitError::Execution("drain approval did not run under the task lock".to_string())
    })
}

/// A test seam just before the drain's approval takes its locks, standing in
/// for a writer that wins the race to them.
#[cfg(test)]
pub(super) mod approval_hook {
    use std::cell::RefCell;

    use crate::OrbitRuntime;

    pub(in super::super) type Hook = Box<dyn FnMut(&OrbitRuntime, &str)>;

    thread_local! {
        static BEFORE_LOCK: RefCell<Option<Hook>> = RefCell::new(None);
    }

    pub(in super::super) fn set_before_lock(hook: Option<Hook>) {
        BEFORE_LOCK.with(|slot| *slot.borrow_mut() = hook);
    }

    pub(super) fn before_lock(runtime: &OrbitRuntime, task_id: &str) {
        BEFORE_LOCK.with(|slot| {
            if let Some(hook) = slot.borrow_mut().as_mut() {
                hook(runtime, task_id);
            }
        });
    }
}
