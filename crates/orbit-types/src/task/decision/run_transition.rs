//! The run that wrote a status transition, read from
//! [`TaskDecision::RunTransition`] (or the run a [`TaskDecision::FinalRecovery`]
//! decision belongs to), with the one legacy reader for entries written
//! before the decision existed.

use super::TaskDecision;
use crate::task::TaskHistoryEntry;

impl TaskHistoryEntry {
    /// The run this entry attributes itself to: the run that shelved the task
    /// in `blocked` or `backlog`, or the final-recovery run that settled it.
    ///
    /// Reads the typed decision. An entry with no decision was written before
    /// it existed (or by an older binary or replica) and names the run only
    /// in its note; the legacy note reader below reads that. Callers decide which
    /// actor, status and event they trust.
    pub fn transition_run_id(&self) -> Option<&str> {
        match &self.decision {
            Some(
                TaskDecision::RunTransition { run_id } | TaskDecision::FinalRecovery { run_id, .. },
            ) => Some(run_id),
            Some(_) => None,
            None => legacy_note_run_id(&self.event, self.note.as_deref()?),
        }
    }
}

/// Compatibility reader for status notes written before
/// [`TaskDecision::RunTransition`]. Each producer named its run in its own
/// grammar, which new entries keep writing byte for byte:
///
/// - workflow failure and interruption: `…, run_id=<id>, …`;
/// - a final-recovery escalation: `final recovery (run_id=<id>) escalated: …`;
/// - a final-recovery requeue, completion, rejection or archive:
///   `final recovery (run_id=<id>): …`;
/// - a run's failure handoff and an expired forge hold: `…: run=<id>,` or
///   `…: run=<id>;`.
///
/// Any other event names no run here.
fn legacy_note_run_id<'a>(event: &str, note: &'a str) -> Option<&'a str> {
    const FINAL_RECOVERY_PREFIX: &str = "final recovery (run_id=";
    match event {
        "workflow_run_failed" | "workflow_run_interrupted" => {
            note.split_once(", run_id=")?.1.split(',').next()
        }
        "final_recovery_escalated" => note
            .strip_prefix(FINAL_RECOVERY_PREFIX)?
            .split_once(") escalated:")
            .map(|(id, _)| id),
        "final_recovery_requeued"
        | "final_recovery_completed"
        | "final_recovery_rejected"
        | "final_recovery_archived" => note
            .strip_prefix(FINAL_RECOVERY_PREFIX)?
            .split_once("):")
            .map(|(id, _)| id),
        "pr_failure_handoff"
        | "pr_conflict_blocked"
        | "validation_environment_blocked"
        | "review_gate_escalation"
        | crate::workflow::FORGE_UNAVAILABLE_EXPIRED_EVENT => {
            let id = note
                .split_once(": run=")?
                .1
                .split([',', ';'])
                .next()?
                .trim();
            (!id.is_empty()).then_some(id)
        }
        _ => None,
    }
}
