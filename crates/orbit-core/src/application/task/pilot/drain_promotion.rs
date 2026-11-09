//! Drain-scoped promotion authority for `orbit run auto --approve-proposed`.
//!
//! A drain started with `approve_proposed` pilots the qualifying `proposed`
//! tasks it selected and hands this boundary its own run ID as the authority
//! record. The record is verified rather than trusted: the named run must be
//! a live auto drain started with the flag, and it must have dispatched the
//! very pilot run applying the results. A task that then passes the shared
//! promotion findings is approved through the ordinary approve transition,
//! with a note naming the drain; any other task stays `proposed` and its pilot
//! history entry records the hold as its typed decision, so the next pass does
//! not pilot it again until the task changes. The approval rechecks the opt-out tag and the
//! pilot-assessed material under the task lock it writes under, so an operator
//! edit racing the drain is never approved over.
//!
//! A `verified_no_diff` assessment of a task not tagged `no-diff-expected`
//! holds it as `pilot_verified_no_diff`, ahead of the readiness gaps its
//! empty selectors leave; the apply step then closes it when it qualifies.
//! A `finding_invalid` assessment holds it as `finding_invalid` ahead of
//! every other finding; the apply step then rejects it.

use std::collections::BTreeSet;

use orbit_common::{NotFoundKind, OrbitError};
use orbit_engine::DispatchError;
use orbit_types::task::{
    NO_DIFF_EXPECTED_TAG, ReadinessGap, ReadinessStage, Task, TaskComplexity, TaskDecision,
    TaskHistoryEntry, TaskStatus, readiness_gaps,
};
use orbit_types::workflow::JobRunTriggerKind;
use orbit_types::workflow::automation::members::PreparationPolicy;
use serde::Serialize;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::legacy_decisions;
use crate::application::task::{
    AdmissionClosure, PILOT_VERIFIED_NO_DIFF, PromotionDecision, TaskAutomationPort, TaskJobPort,
    VerifiedNoDiff,
};

use super::apply::admission::{AppliedTaskMaterial, PreparedTaskSnapshot, ValidatedTask};
use super::persist::{assessed_material_drift, with_task_locks};
use super::promotion::{
    FOOTPRINT_UNRESOLVED, PromotionFindings, auto_approval_hold_classification,
    auto_approval_opted_out, footprint_hold, promotion_findings, promotion_task_ids, warning_text,
};

const DRAIN_JOB: &str = "workspace_auto_pipeline";
const PILOT_JOB: &str = "task_pilot_pipeline";

/// Why a `proposed` task does not qualify for drain approval, before any
/// pilot runs: a `no-auto-approve` tag excludes it outright; otherwise its
/// first blocking readiness gap ([`readiness_gaps`]) withholds it.
pub(crate) fn approval_disqualification(
    tags: &[String],
    context_files: &[String],
    complexity: Option<TaskComplexity>,
) -> Option<&'static str> {
    if auto_approval_opted_out(tags) {
        return Some(auto_approval_hold_classification(tags));
    }
    readiness_gaps(ReadinessStage::Proposed, tags, context_files, complexity)
        .into_iter()
        .find(ReadinessGap::is_blocking)
        .map(|gap| gap.code.as_str())
}

/// The approve-transition note; task history names the approving drain.
pub(crate) fn approval_note(run_id: &str) -> String {
    format!("approved by auto drain {run_id} after task-pilot verification")
}

/// Approve `task_id` as drain `drain_run_id`, recording the approving drain
/// (and the reused pilot receipt's `operation_id`) as a typed decision on the
/// `proposal_approved` event. The note text history views show is unchanged.
fn approve_as_drain(
    runtime: &OrbitRuntime,
    task_id: &str,
    drain_run_id: &str,
    operation_id: Option<&str>,
) -> Result<(), OrbitError> {
    let note = match operation_id {
        Some(operation_id) => format!("{} (operation {operation_id})", approval_note(drain_run_id)),
        None => approval_note(drain_run_id),
    };
    runtime.approve_task_with_decision(
        task_id,
        Some(note),
        Some(TaskDecision::DrainApproved {
            drain_run_id: drain_run_id.to_string(),
            operation_id: operation_id.map(str::to_string),
        }),
    )?;
    Ok(())
}

/// Whether a `proposal_approved` history entry records drain `run_id`'s
/// approval: its typed decision, or the note of an entry written before
/// drain approvals carried one.
pub(crate) fn approved_by_drain(entry: &TaskHistoryEntry, run_id: &str) -> bool {
    match &entry.decision {
        Some(TaskDecision::DrainApproved { drain_run_id, .. }) => drain_run_id == run_id,
        Some(_) => false,
        None => legacy_decisions::drain_approval_note_names(entry, run_id),
    }
}

/// The pilot classification a drain hold recorded on a `task_pilot_applied`
/// history entry. Its note is read for one only when `runtime`'s store
/// predates typed decisions: the pilot agent's rationale is part of it.
pub(crate) fn held_classification<'a>(
    runtime: &OrbitRuntime,
    entry: &'a TaskHistoryEntry,
) -> Option<&'a str> {
    match &entry.decision {
        Some(TaskDecision::DrainApprovalHeld { classification }) => Some(classification),
        Some(_) => None,
        None => {
            legacy_decisions::drain_held_classification(entry, &|| runtime.typed_decisions_since())
        }
    }
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
            .ok_or_else(|| {
                DispatchError::action_failed(action, "drain_promotion.run_id must be a string")
            })?;
        let refused = |reason: &str| {
            DispatchError::action_failed(
                action,
                format!("drain promotion authority from run {run_id} refused: {reason}"),
            )
        };
        let drain = runtime
            .stores()
            .jobs()
            .get_job_run(run_id)
            .map_err(|error| DispatchError::action_failed(action, error.to_string()))?
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
            .map_err(|error| DispatchError::action_failed(action, error.to_string()))?
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

/// A drain admission record, with approval and closure appended in persisted order.
#[derive(Serialize)]
pub(super) struct DrainAdmissionDecision {
    pub(super) task_id: String,
    pub(super) decision: PromotionDecision,
    pub(super) classification: String,
    pub(super) drain_run_id: String,
    pub(super) evidence: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) approved: Option<bool>,
    #[serde(flatten)]
    pub(super) closure: AdmissionClosure,
}

/// The task, its pilot assessment and its snapshot that one drain admission
/// decides over.
pub(super) struct DrainAdmissionTask<'a> {
    pub(super) action: &'a str,
    pub(super) task_id: &'a str,
    pub(super) snapshot: &'a PreparedTaskSnapshot,
    pub(super) current: &'a Task,
    pub(super) assessment: &'a Value,
    pub(super) selectors: &'a [String],
    pub(super) complexity: TaskComplexity,
    pub(super) dependencies: &'a [String],
    pub(super) is_done: &'a dyn Fn(&str) -> bool,
}

/// The first promotion condition a task fails. Fresh apply records
/// `classification` and `evidence`; certified reuse records `classification`
/// and `detail` (footprint holds only).
struct PromotionHold {
    classification: &'static str,
    evidence: Value,
    detail: Option<String>,
}

/// Classify one task under the drain promotion policy, in the single priority
/// order fresh apply and certified reuse share. `None` means it promotes.
///
/// The inputs come from each caller's own source: readiness reads `snapshot`,
/// the opt-out and status read `current`, and `selectors` and `complexity` are
/// the assessed values. Certified reuse passes a snapshot built from the task
/// it certifies, so its readiness and no-diff reads match the task's.
fn classify_promotion(
    snapshot: &PreparedTaskSnapshot,
    current: &Task,
    assessment: &Value,
    findings: &PromotionFindings<'_>,
    selectors: &[String],
    complexity: Option<TaskComplexity>,
) -> Option<PromotionHold> {
    let hold = |classification: &'static str, evidence: Value| {
        Some(PromotionHold {
            classification,
            evidence,
            detail: None,
        })
    };
    let disposition = assessment
        .get("disposition")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let no_diff = snapshot.tags.iter().any(|tag| tag == NO_DIFF_EXPECTED_TAG);
    if current.status != TaskStatus::Proposed {
        return hold("not_proposed", json!(format!("task is {}", current.status)));
    }
    // Apply rejects the task once the pilot write lands; never approve it.
    if let Some(finding) = assessment
        .get(super::FINDING_INVALID)
        .filter(|finding| !finding.is_null())
    {
        return hold(super::FINDING_INVALID, finding.clone());
    }
    if auto_approval_opted_out(&current.tags) {
        return hold(
            auto_approval_hold_classification(&current.tags),
            Value::Null,
        );
    }
    if !no_diff && let Some(finding) = VerifiedNoDiff::from_assessment(assessment) {
        return hold(PILOT_VERIFIED_NO_DIFF, finding.to_json());
    }
    if let Some(reason) =
        approval_disqualification(&snapshot.tags, &snapshot.context_files, snapshot.complexity)
    {
        return hold(reason, Value::Null);
    }
    if !findings.already_landed.is_null() {
        return hold("already_landed", findings.already_landed.clone());
    }
    if !findings.duplicate_of.is_null() {
        return hold("duplicate", findings.duplicate_of.clone());
    }
    if let Some(finding) = assessment
        .get("release_action_required")
        .filter(|finding| !finding.is_null())
    {
        return hold("release_action_required", finding.clone());
    }
    if let Some(finding) = footprint_hold(assessment) {
        return Some(PromotionHold {
            classification: FOOTPRINT_UNRESOLVED,
            evidence: finding.clone(),
            detail: warning_text(finding).map(str::to_string),
        });
    }
    if !findings.warnings.is_empty() {
        return hold("warnings", json!(findings.warnings));
    }
    if !no_diff && (disposition != "selectors" || selectors.is_empty()) {
        return hold(
            "no_actionable_selectors",
            assessment.get("evidence").cloned().unwrap_or(Value::Null),
        );
    }
    if !no_diff && !complexity.is_some_and(TaskComplexity::is_assessed) {
        return hold("unassessed_complexity", Value::Null);
    }
    None
}

/// Decide one task under drain authority. Pilot output that is malformed
/// fails the task; every well-formed finding becomes a classified hold.
pub(super) fn assess_drain_admission(
    task: DrainAdmissionTask<'_>,
    authority: &DrainAuthority,
) -> Result<Box<DrainAdmissionDecision>, DispatchError> {
    let DrainAdmissionTask {
        action,
        task_id,
        snapshot,
        current,
        assessment,
        selectors,
        complexity,
        dependencies,
        is_done,
    } = task;
    let findings = promotion_findings(action, task_id, assessment, dependencies, is_done)?;
    let (decision, classification, evidence) = match classify_promotion(
        snapshot,
        current,
        assessment,
        &findings,
        selectors,
        Some(complexity),
    ) {
        Some(hold) => (
            PromotionDecision::Withhold,
            hold.classification,
            hold.evidence,
        ),
        None => (
            PromotionDecision::Promote,
            "verified_ready",
            json!(
                "pilot validated the task with no duplicate, already-landed, conflict, or warning finding"
            ),
        ),
    };
    Ok(Box::new(DrainAdmissionDecision {
        task_id: task_id.to_string(),
        decision,
        classification: classification.to_string(),
        drain_run_id: authority.run_id.clone(),
        evidence,
        approved: None,
        closure: Default::default(),
    }))
}

/// The typed decision a hold records on the pilot's own history entry.
pub(super) fn drain_admission_hold_decision(
    admission: &DrainAdmissionDecision,
) -> Option<TaskDecision> {
    match admission.decision {
        PromotionDecision::Withhold => Some(TaskDecision::DrainApprovalHeld {
            classification: admission.classification.clone(),
        }),
        PromotionDecision::Promote => None,
    }
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

/// Whether a dependency read through its registered owner counts as done,
/// including the shared archived-after-done rule.
fn dependency_is_done(runtime: &OrbitRuntime, target: &Task) -> bool {
    if target.status == TaskStatus::Done {
        return true;
    }
    target.status == TaskStatus::Archived
        && runtime
            .get_dependency_history(&target.id)
            .ok()
            .flatten()
            .is_some_and(|history| {
                orbit_types::task::archived_task_completed_before_archive(&history)
            })
}

/// Approve a promoted task through the ordinary approve transition. The
/// decision is made under the task and dependency locks the pilot write took,
/// and the transition runs inside them, so a concurrent edit either lands
/// before the check or waits behind the approval. A task tagged
/// `no-auto-approve` since it was assessed is held, as is one whose
/// pilot-assessed material changed after the pilot write: the next drain pass
/// pilots it again. Named prerequisites without stored edges must still be
/// done under their own locks; an unlocked earlier completion read cannot
/// authorize approval.
pub(super) fn approve(
    runtime: &OrbitRuntime,
    validated: &ValidatedTask,
    snapshot: &PreparedTaskSnapshot,
    policy: &PreparationPolicy,
    authority_run_id: &str,
) -> Result<Approval, OrbitError> {
    let task_id = validated.task_id.as_str();
    for attempt in 0..4 {
        let mut lock_ids = vec![task_id.to_string()];
        // A dependency owned by another registered workspace is external:
        // it takes no lock in this store, and its status comes from the
        // registered-owner lookup `resolved_dependencies` uses. A
        // dependency no registered workspace holds is the only `task not
        // found` [ORB-21172].
        let mut external_done = BTreeSet::new();
        for id in runtime.get_task(task_id)?.dependencies() {
            if runtime.stores().tasks().get_task(&id)?.is_some() {
                lock_ids.push(id);
            } else if let Some(target) = runtime.get_dependency_task(&id)? {
                if dependency_is_done(runtime, &target) {
                    external_done.insert(id);
                }
            } else {
                return Err(OrbitError::not_found(NotFoundKind::Task, id));
            }
        }
        let named = promotion_task_ids(&validated.assessment);
        // A prerequisite that was done at the pilot write has no stored edge.
        // Lock those named local tasks too before checking completion again.
        for id in &named {
            if runtime.stores().tasks().get_task(id)?.is_some() {
                lock_ids.push(id.clone());
            } else if let Ok(Some(target)) = runtime.get_dependency_task(id)
                && dependency_is_done(runtime, &target)
            {
                external_done.insert(id.clone());
            }
        }
        lock_ids.sort();
        lock_ids.dedup();
        #[cfg(test)]
        approval_hook::before_lock(runtime, task_id);
        let mut approval = None;
        let mut entered = false;
        let result = with_task_locks(runtime, task_id, &lock_ids, 0, &mut || {
            entered = true;
            let task = runtime.get_task(task_id)?;
            let mut done = external_done.clone();
            for id in &named {
                // A task absent during discovery is held conservatively without
                // reading a newly appeared task outside the sorted lock set.
                if lock_ids.contains(id)
                    && runtime
                        .stores()
                        .tasks()
                        .get_task(id)?
                        .is_some_and(|target| target.status == TaskStatus::Done)
                {
                    done.insert(id.clone());
                }
            }
            let warnings = promotion_findings(
                "apply_task_pilot_results",
                task_id,
                &validated.assessment,
                &task.dependencies(),
                &|id| done.contains(id),
            )
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?
            .warnings;
            approval = Some(if task.status != TaskStatus::Proposed {
                Approval::NotProposed
            } else if auto_approval_opted_out(&task.tags) {
                Approval::Held {
                    classification: auto_approval_hold_classification(&task.tags),
                    evidence: Value::Null,
                }
            } else if let Some((reason, detail)) =
                assessed_material_drift(runtime, &task, snapshot, validated, policy)
            {
                Approval::Held {
                    classification: "changed_since_pilot",
                    evidence: json!({ "reason": reason, "detail": detail }),
                }
            } else if !warnings.is_empty() {
                Approval::Held {
                    classification: "warnings",
                    evidence: json!(warnings),
                }
            } else {
                approve_as_drain(runtime, task_id, authority_run_id, None)?;
                Approval::Approved
            });
            Ok(())
        });
        if !entered && matches!(&result, Err(OrbitError::NotFound { .. })) && attempt < 3 {
            // A named prerequisite vanished before its lock. Rebuild the sorted
            // set; absence now holds the finding without taking an absent lock.
            continue;
        }
        result?;
        return approval.ok_or_else(|| {
            OrbitError::Execution("drain approval did not run under the task lock".to_string())
        });
    }
    Err(OrbitError::Execution(
        "drain approval lock discovery did not settle".to_string(),
    ))
}

/// What a drain pass should do with an explicit or routine pilot receipt.
///
/// `NotReusable` leaves the ordinary candidate path alone: a drain child's
/// own assessment, a run with no recorded trigger, and a history note with
/// no receipt are not this identity.
pub(crate) enum ExplicitAssessmentReuse {
    /// Fresh certified assessment. Approve it without another provider call.
    Promote { operation_id: String },
    /// Certified assessment that must stay proposed. Do not pilot it again.
    Hold {
        reason: &'static str,
        detail: Option<String>,
    },
    /// No host-owned explicit or routine assessment to consume.
    NotReusable,
}

/// Whether `pilot_run_id` is a child another run dispatched.
///
/// The trigger is a host write at admission. `task_ids` and
/// `promotion_authorized` on the run input are not authority. A child is
/// never an operator request, whether or not its parent is still live.
pub(crate) fn dispatched_child_pilot(
    runtime: &OrbitRuntime,
    pilot_run_id: Option<&str>,
) -> Result<bool, OrbitError> {
    let Some(pilot_run_id) = pilot_run_id
        .map(str::trim)
        .filter(|run_id| !run_id.is_empty())
    else {
        return Ok(false);
    };
    Ok(runtime
        .stored_run_state(pilot_run_id)?
        .and_then(|state| state.trigger)
        .is_some_and(|trigger| trigger.kind == JobRunTriggerKind::Child))
}

/// The tasks a live execution claim holds, footprint-protecting phases only.
/// The claim guard both stale-repilot selection sites share: while a claim
/// holds a task, its meaning is still being executed against, so a stale
/// assessment keeps holding instead of queueing a fresh pilot.
pub(crate) fn live_execution_claim_tasks(
    runtime: &OrbitRuntime,
) -> Result<std::collections::BTreeSet<String>, OrbitError> {
    Ok(runtime
        .inspect_execution_claims()?
        .into_iter()
        .map(|inspection| inspection.claim)
        .filter(|claim| claim.phase.protects_footprint())
        .map(|claim| claim.task_id)
        .collect())
}

/// Why a child pilot must not call the provider for `task`, if it must not.
///
/// An explicit or routine pilot already certified an assessment: a fresh one
/// is `assessment_already_applied`, a held one names its hold. A drain-owned
/// receipt stays on the ordinary prepare path, so a material change found at
/// that child's own approval can still be piloted again.
pub(crate) fn reused_assessment_exclusion(
    runtime: &OrbitRuntime,
    task: &Task,
) -> Result<Option<&'static str>, OrbitError> {
    Ok(match classify_reuse(runtime, task, false)? {
        ExplicitAssessmentReuse::Promote { .. } => Some("assessment_already_applied"),
        ExplicitAssessmentReuse::Hold { reason, .. } => {
            // A stale assessment is not reused, but it is exactly what a
            // drain child dispatched for a repilot must assess afresh: the
            // drain selected the task because its meaning changed after the
            // certified assessment. A live execution claim keeps the hold.
            if reason == "changed_since_pilot"
                && !live_execution_claim_tasks(runtime)?.contains(&task.id)
            {
                None
            } else {
                Some(reason)
            }
        }
        ExplicitAssessmentReuse::NotReusable => None,
    })
}

/// Approve one fresh explicit or routine assessment under the task lock.
///
/// Returns whether this call moved the task to `backlog`. A concurrent
/// edit that makes the receipt stale, or a pilot that started since the
/// snapshot, leaves the task proposed.
pub(crate) fn promote_reusable_assessment(
    runtime: &OrbitRuntime,
    drain_run_id: &str,
    task_id: &str,
) -> Result<bool, OrbitError> {
    let mut promoted = false;
    with_task_locks(runtime, task_id, &[task_id.to_string()], 0, &mut || {
        // The selection input is not promotion authority. Verify the live
        // host-owned drain record again at the write boundary.
        let Some(drain) = runtime.stores().jobs().get_job_run(drain_run_id)? else {
            return Ok(());
        };
        if drain.job_id != DRAIN_JOB
            || drain.state.is_terminal()
            || drain
                .input
                .as_ref()
                .and_then(|input| input.get("approve_proposed"))
                .and_then(Value::as_bool)
                != Some(true)
        {
            return Ok(());
        }
        let task = runtime.get_task(task_id)?;
        if task.status != TaskStatus::Proposed {
            return Ok(());
        }
        let ExplicitAssessmentReuse::Promote { operation_id } =
            classify_explicit_assessment_reuse(runtime, &task)?
        else {
            return Ok(());
        };
        approve_as_drain(runtime, task_id, drain_run_id, Some(&operation_id))?;
        promoted = true;
        Ok(())
    })?;
    Ok(promoted)
}

/// Classify the latest task-pilot receipt for drain reuse.
///
/// The receipt's `pilot_run_id` is the host-owned identity. Only a run
/// whose admission trigger is `cli`, `mcp`, `dashboard`, or `routine` can
/// be consumed. A `child` trigger, or a run with no trigger, is not an
/// operator or routine request.
pub(crate) fn classify_explicit_assessment_reuse(
    runtime: &OrbitRuntime,
    task: &Task,
) -> Result<ExplicitAssessmentReuse, OrbitError> {
    classify_reuse(runtime, task, true)
}

/// [`classify_explicit_assessment_reuse`], optionally without the live-pilot
/// hold. A child's own prepare is itself a live pilot of the task, and has
/// already excluded every other run preparing it.
fn classify_reuse(
    runtime: &OrbitRuntime,
    task: &Task,
    hold_live_pilot: bool,
) -> Result<ExplicitAssessmentReuse, OrbitError> {
    let comments = runtime.get_task_pilot_comments(&task.id)?;
    let Some((_, receipt)) = super::pilot_receipts(&comments).next() else {
        return Ok(ExplicitAssessmentReuse::NotReusable);
    };
    if !super::is_operation_id(receipt.operation_id) {
        return Ok(ExplicitAssessmentReuse::NotReusable);
    }
    let Ok(audit) = serde_json::from_str::<Value>(receipt.body) else {
        return Ok(ExplicitAssessmentReuse::NotReusable);
    };
    let Some(assessment) = audit.get("assessment").filter(|value| value.is_object()) else {
        return Ok(ExplicitAssessmentReuse::NotReusable);
    };
    let Some(pilot_run_id) = audit
        .get("pilot_run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|run_id| !run_id.is_empty())
    else {
        return Ok(ExplicitAssessmentReuse::NotReusable);
    };
    let trigger = runtime
        .stored_run_state(pilot_run_id)?
        .and_then(|state| state.trigger);
    if !trigger
        .as_ref()
        .is_some_and(|recorded| reusable_pilot_trigger(recorded.kind))
    {
        return Ok(ExplicitAssessmentReuse::NotReusable);
    }
    // A receipt written before the audit certified its material and
    // complexity (#4841) cannot be checked for drift, so it is not this
    // identity: the drain pilots the task as usual.
    let Some(written) = audit
        .get("applied_material")
        .and_then(|value| serde_json::from_value::<AppliedTaskMaterial>(value.clone()).ok())
    else {
        return Ok(ExplicitAssessmentReuse::NotReusable);
    };
    let Some(complexity) = audit
        .get("complexity_after")
        .and_then(|value| serde_json::from_value::<TaskComplexity>(value.clone()).ok())
    else {
        return Ok(ExplicitAssessmentReuse::NotReusable);
    };
    if hold_live_pilot && let Some(run_id) = runtime.live_pilot_run(&task.id)? {
        return Ok(ExplicitAssessmentReuse::Hold {
            reason: "pilot_running",
            detail: Some(format!("task-pilot run {run_id} is assessing it")),
        });
    }
    // Selection lists envelopes. Those omit description, acceptance criteria
    // and plan, so a freshness or fingerprint check against the envelope is a
    // false edit. The receipt seals the stored body.
    let task = runtime.get_task(&task.id)?;
    let task = &task;
    let operation_id = receipt.operation_id;
    let history = || runtime.get_task_history(&task.id);
    if super::freshness::pilot_requirements_stale(task, &audit, operation_id, &history)? {
        return Ok(ExplicitAssessmentReuse::Hold {
            reason: "changed_since_pilot",
            detail: Some("description or acceptance criteria changed after the assessment".into()),
        });
    }
    let policy = runtime.claim_preparation_policy(None)?;
    let validated = certified_validated_task(
        task,
        assessment.clone(),
        written,
        complexity,
        operation_id.to_string(),
        pilot_run_id.to_string(),
    );
    let snapshot = certified_snapshot(task);
    if let Some((reason, detail)) =
        assessed_material_drift(runtime, task, &snapshot, &validated, &policy)
    {
        return Ok(ExplicitAssessmentReuse::Hold {
            reason: "changed_since_pilot",
            detail: Some(format!("{reason}: {detail}")),
        });
    }
    promotable_reuse(runtime, task, &snapshot, assessment, operation_id)
}

fn reusable_pilot_trigger(kind: JobRunTriggerKind) -> bool {
    matches!(
        kind,
        JobRunTriggerKind::Cli
            | JobRunTriggerKind::Mcp
            | JobRunTriggerKind::Dashboard
            | JobRunTriggerKind::Routine
    )
}

fn certified_snapshot(task: &Task) -> PreparedTaskSnapshot {
    PreparedTaskSnapshot {
        context_files: task.context_files.clone(),
        status: TaskStatus::Proposed,
        complexity: task.complexity,
        title: task.title.clone(),
        tags: task.tags.clone(),
        material: None,
        status_neutral_fingerprint: None,
        material_components: None,
        history_len: None,
        validation_tool_warnings: Vec::new(),
        context_creation_identity: None,
        pilot_mode: Default::default(),
    }
}

fn certified_validated_task(
    task: &Task,
    assessment: Value,
    written: AppliedTaskMaterial,
    complexity: TaskComplexity,
    operation_id: String,
    pilot_run_id: String,
) -> ValidatedTask {
    // Selector authority is the sealed fingerprint inside `written`. Using
    // the task's current selectors here keeps apply's retained grants from
    // looking like drift; a later selector edit still fails the fingerprint.
    ValidatedTask {
        task_id: task.id.clone(),
        after: task.context_files.clone(),
        assessment,
        admission: None,
        promote: false,
        history_decision: None,
        complexity,
        operation_id,
        required_os: Vec::new(),
        required_machine: None,
        post_deploy: Vec::new(),
        user_facing_ui: false,
        applied_material: Some(written),
        pilot_run_id: Some(pilot_run_id),
        pilot_crew: None,
        pilot_auto_crew: None,
    }
}

fn promotable_reuse(
    runtime: &OrbitRuntime,
    task: &Task,
    snapshot: &PreparedTaskSnapshot,
    assessment: &Value,
    operation_id: &str,
) -> Result<ExplicitAssessmentReuse, OrbitError> {
    let dependencies = task.dependencies();
    let findings = match promotion_findings(
        "select_proposed_approvals",
        &task.id,
        assessment,
        &dependencies,
        &|id| runtime.warning_prerequisite_is_done(id),
    ) {
        Ok(findings) => findings,
        Err(error) => {
            return Ok(ExplicitAssessmentReuse::Hold {
                reason: "pilot_output_malformed",
                detail: Some(error.to_string()),
            });
        }
    };
    Ok(
        match classify_promotion(
            snapshot,
            task,
            assessment,
            &findings,
            &task.context_files,
            task.complexity,
        ) {
            Some(hold) => ExplicitAssessmentReuse::Hold {
                reason: hold.classification,
                detail: hold.detail,
            },
            None => ExplicitAssessmentReuse::Promote {
                operation_id: operation_id.to_string(),
            },
        },
    )
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
