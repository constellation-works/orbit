//! Readers for decisions recorded as text before [`TaskDecision`] existed.
//!
//! Decisions automation acts on now have typed history fields or audit
//! receipts. Records written before them carry the decision in text; these readers
//! are the only place that text is parsed, and each is consulted only where a
//! new record cannot reach it:
//!
//! - a `[no-target-assessed:` history-note marker is read only when the
//!   matching pilot audit has no typed targetless assessment (or no matching
//!   receipt exists). This is assessment evidence, not an operator decision.
//! - a `task_pilot_unresolved` event's `{"pilot_run_id": ...}` JSON note is
//!   read only when the entry has no typed decision. Only `system` writes
//!   that event, through the task store, so its note is not open to an
//!   untrusted writer.
//! - a `proposal_approved` event's `approved by auto drain <run>` note names
//!   the approving drain only when the entry has no typed decision. Drains
//!   record `DrainApproved` since; the note reader keeps the presence rule
//!   it had, so attribution of an older approval does not change.
//! - a `host_operational_held` or `operator_validation_held` event's
//!   `{"operation_id": ..., "hold": ...}` JSON note names the handed-off
//!   pilot receipt and hold only when the entry has no typed decision. Only
//!   `system` and `task-pilot` write those events, so the note keeps the
//!   rules it had: hold recovery trusts only those authors, and a recorded
//!   handoff is not posted again.
//! - a workflow admission's `started` event, a `system`-authored
//!   transition to `in-progress` with a `workflow admission: <workflow>`
//!   note, is read from that note only when the entry has no typed
//!   decision. Admission records `WorkflowAdmission` since; only `system`
//!   writes that transition through admission, so the note keeps the
//!   presence rule the pilot supersession check had.
//!
//! The final-recovery comment, operator override and resolution, and drain
//! hold readers, with the store's typed-decisions boundary file they relied
//! on, were retired 30 days after their typed fields shipped (2026-10-12).
//! The readers above were typed on 2026-11-07; by the same rule they become
//! retirable from 2026-12-07, each once its own condition below holds.
//! Retire the targetless assessment adapter and dual-written marker once
//! every live task's latest targetless pilot assessment has a typed audit
//! receipt and no supported older binary can still write marker-only records.
//! Retire the undecided-pilot note reader once no live proposed or backlog
//! task holds a `task_pilot_unresolved` entry without a typed decision.
//! Retire the operator-handoff note reader once no live backlog task holds a
//! handoff entry without a typed `PilotOperatorHandoff` decision.
//! Retire the drain-approval note reader once no drain run still reports on
//! a task approved without a typed `DrainApproved` decision.
//! Retire the workflow-admission note reader once no in-flight pilot
//! preparation predates its task's typed `WorkflowAdmission` decision.
//!
//! [`TaskDecision`]: orbit_types::task::TaskDecision

use orbit_types::task::{TaskHistoryEntry, TaskStatus};
use serde_json::Value;

use super::helpers::SYSTEM_ACTOR_LABEL;

/// Compatibility reader for marker-only targetless pilot assessments. Called
/// only when the latest applied event has no typed assessment receipt; retire
/// with the dual-read/dual-write condition in this module's documentation.
pub(super) fn no_target_pilot_assessment_marker(note: &str) -> Option<(TaskStatus, &str)> {
    let (_, marker) = note.split_once(" [no-target-assessed:")?;
    let (marker, _) = marker.split_once(']')?;
    let (status, fingerprint) = marker.split_once(':')?;
    if fingerprint.len() != 64 || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some((status.parse().ok()?, fingerprint))
}

/// The run a `task_pilot_unresolved` entry written before its typed decision
/// names in its JSON note. Callers consult it only for an entry without one.
pub(crate) fn unresolved_pilot_note_run_id(entry: &TaskHistoryEntry) -> Option<String> {
    let note: Value = serde_json::from_str(entry.note.as_deref()?).ok()?;
    note["pilot_run_id"].as_str().map(str::to_string)
}

/// Whether a `proposal_approved` entry written before drain approvals
/// carried a typed decision names drain `run_id` in its note. Callers consult
/// it only for an entry without a decision.
pub(crate) fn drain_approval_note_names(entry: &TaskHistoryEntry, run_id: &str) -> bool {
    entry
        .note
        .as_deref()
        .is_some_and(|note| note.starts_with(&super::pilot::approval_note(run_id)))
}

/// The pilot receipt and hold an operator handoff entry written before its
/// typed decision names in its JSON note. Callers consult it only for an
/// entry without one.
pub(crate) fn pilot_operator_handoff_note(entry: &TaskHistoryEntry) -> Option<(String, Value)> {
    let record: Value = serde_json::from_str(entry.note.as_deref()?).ok()?;
    let operation_id = record["operation_id"].as_str()?.to_string();
    Some((operation_id, record["hold"].clone()))
}

/// Whether a decision-less history entry is a workflow admission written
/// before admission recorded [`TaskDecision::WorkflowAdmission`]: a
/// `system`-authored transition to `in-progress` whose note names the
/// workflow. Callers consult it only for an entry without a decision.
///
/// [`TaskDecision::WorkflowAdmission`]: orbit_types::task::TaskDecision::WorkflowAdmission
pub(crate) fn workflow_admission_note(entry: &TaskHistoryEntry) -> bool {
    entry.to_status == Some(TaskStatus::InProgress)
        && entry.by == SYSTEM_ACTOR_LABEL
        && entry
            .note
            .as_deref()
            .is_some_and(|note| note.starts_with("workflow admission:"))
}
