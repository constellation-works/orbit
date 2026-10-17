//! Deterministic support actions for the task-pilot workflow [ORB-10510].
//!
//! The agent leg only proposes task metadata. These actions own discovery,
//! partitioning and canonical selector validation. Ordinarily the sole write
//! is persisting assessed complexity and replacing `context_files` on the exact
//! tasks prepared for the run, with the assessment audit and replay receipt in
//! the same task-bundle commit. A CI-failure sweep may additionally request
//! explicit admission: after the selectors and every recommendation validate,
//! this boundary promotes only a current, warning-free repair from `proposed`
//! to `backlog`. An `--approve-proposed` drain requests the same promotion
//! under its own verified authority, through the ordinary approve transition.
//!
//! A selector that does not exist at the pinned source is accepted only when
//! the task's durable creation grant names it exactly. Preparation records
//! the grant's identity, apply refuses a task whose grant changed since, and
//! the atomic write compares it again under the task lock. The pilot cannot
//! drop a granted target: apply keeps any it omitted.
//!
//! A review-filed task's selectors are the review's evidence [ORB-15285]. On
//! a `selectors` assessment apply keeps every one the pilot omitted that
//! still resolves at the pinned source, in the filed order, so re-assessment
//! cannot narrow it and an unchanged scope is not rewritten. Only an
//! operator's `orbit.task.update` narrows it.
//!
//! A typed host finding routes the task in the same commit [ORB-15278]: a
//! `required_os` finding naming one OS adds its `os:` tag to a task that
//! carries none (an operator's `os:` tags are never touched), and a
//! `required_machine` finding is recorded with the native-host hold, so pull
//! admission refuses every other machine.
//!
//! An applied assessment also holds or gates the task: backlog holds from
//! applied findings (`admission`), prerequisite resolution (`blockers`) and
//! verified-no-diff holds (`no_diff`).

mod admission;
mod apply;
mod apply_admission;
mod assessment;
mod attachment_budget;
mod blockers;
mod drain_promotion;
mod freshness;
pub(crate) mod input;
mod no_diff;
mod persist;
mod prepare;
mod promotion;
pub(crate) mod source;
mod validation_tools;

pub(crate) use admission::{
    HostOperationalHold, MachineRequirement, NativeOsHold, NativeOsRequirement,
    OperatorValidationHold, OperatorValidationRequirement, PilotAdmissionHold,
    operator_validation_requirements,
};
pub(crate) use apply::apply;
pub(super) use assessment::member_ready;
use assessment::{
    normalize_evidence_gaps, required_machine, required_os, retained_review_evidence, review_filed,
    routing_os, unauthorized_missing_targets, validate_after_selectors, validate_recommendations,
};
pub(crate) use blockers::{normalize_pilot_self_blockers, pilot_blocker_task_ids};
pub(crate) use drain_promotion::{
    approval_disqualification, approved_by_drain, held_classification,
};
use input::{requested_workspace_root, required_string, required_string_array};
pub(crate) use no_diff::{NoDiffClosure, PILOT_VERIFIED_NO_DIFF, VerifiedNoDiff};
pub(crate) use prepare::prepare;
pub(crate) use promotion::{
    PromotionFindings, auto_approval_opted_out, promotion_findings, recommendation_has_evidence,
};
pub(crate) use source::requested_base_branch;

use orbit_types::task::{CommentReceipt, TaskComment, TaskStatus};
use serde_json::{Value, json};

/// Actor that seals a task-pilot replay receipt on its audit comment.
const PILOT_ACTOR: &str = "task-pilot";

/// Task-pilot audit comments that carry a receipt, newest first.
///
/// Double-ended so `applied_material` can walk oldest first. Operation ids
/// are not proven unique on historical comments: the store refuses a second
/// atomic apply of the same event type, and a legacy text receipt can still
/// share an id with a later typed comment.
fn pilot_receipts(
    comments: &[TaskComment],
) -> impl DoubleEndedIterator<Item = (&TaskComment, CommentReceipt<'_>)> + '_ {
    comments.iter().rev().filter_map(|comment| {
        if comment.by != PILOT_ACTOR {
            return None;
        }
        comment.receipt().map(|receipt| (comment, receipt))
    })
}

/// Operation id of the newest task-pilot receipt comment.
fn latest_pilot_operation_id(comments: &[TaskComment]) -> Option<&str> {
    pilot_receipts(comments)
        .next()
        .map(|(_, receipt)| receipt.operation_id)
}

/// Whether `value` is 64 ASCII hex digits.
///
/// That is the shape of a pilot operation id. The no-target assessment
/// marker stores its fingerprint in the same shape and uses this check.
fn is_operation_id(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Superseded task outcome whose object is `{task_id, outcome, reason, status, detail}`.
///
/// `source_superseded` omits `status`, and a task held by another pilot names
/// `run_id` instead. Those shapes stay at their call sites.
fn superseded_outcome(task_id: &str, reason: &str, status: TaskStatus, detail: &str) -> Value {
    json!({
        "task_id": task_id,
        "outcome": "superseded",
        "reason": reason,
        "status": status,
        "detail": detail,
    })
}

/// Field carrying the deterministic validation-tool feasibility findings, on
/// both a prepared task snapshot and the assessment apply reports for it
/// [ORB-11980].
pub(super) const VALIDATION_TOOL_WARNINGS: &str = "validation_tool_warnings";

/// Prepared-snapshot fields carrying the task's durable context creation
/// grant: the exact selectors it authorizes and the identity of the record.
pub(super) const CONTEXT_CREATION_SELECTORS: &str = "context_creation_selectors";
pub(super) const CONTEXT_CREATION_IDENTITY: &str = "context_creation_identity";

/// Assessment fields apply attaches: granted creation targets the pilot
/// omitted and apply kept, and dropped missing selectors no grant covers,
/// each naming the operator reauthorization path.
pub(super) const CONTEXT_CREATION_RETAINED: &str = "context_creation_retained";
pub(super) const CONTEXT_REAUTHORIZATION_REQUIRED: &str = "context_reauthorization_required";

/// Assessment field apply attaches: a review-filed task's evidence selectors
/// the pilot omitted and apply kept.
pub(super) const CONTEXT_EVIDENCE_RETAINED: &str = "context_evidence_retained";

/// Assessment field apply attaches when it rewrote a model formatting slip
/// (today a scalar or missing `evidence_gaps`) instead of rejecting it.
pub(super) const PILOT_NORMALIZATIONS: &str = "pilot_normalizations";

#[cfg(test)]
mod tests;
