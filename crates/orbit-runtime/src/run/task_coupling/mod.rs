//! The tasks coupled to a run: releasing a run's reservations, classifying
//! stale ones, the resume lineage a coupled task records, and provider holds.

pub mod provider_hold;
mod reservation_cleanup;

use orbit_types::task::TaskHistoryEntry;

pub use reservation_cleanup::StaleTaskReservation;

/// History event a final-recovery requeue records; cleanup preserves that
/// decision and the applier counts these events toward its requeue bound.
pub const FINAL_RECOVERY_REQUEUED_EVENT: &str = "final_recovery_requeued";

impl crate::RuntimeKernel {
    /// Whether a task's linked run may be read from this machine's store.
    ///
    /// Run IDs are machine-local. An explicit binding must match this
    /// runtime's identity; an unknown identity cannot establish a match.
    /// Legacy records without a machine binding retain their local behavior.
    pub fn task_run_is_local(&self, task: &orbit_types::task::Task) -> bool {
        task.job_run_machine.as_ref().is_none_or(|location| {
            self.automation_machine_identity() == Some(location.machine_id.as_str())
        })
    }
}

/// The latest resume's cleanup owner, only while its recorded batch binding
/// still matches the task. A later restamp invalidates the old coupling.
pub fn resumed_task_run_id<'a>(
    history: &'a [TaskHistoryEntry],
    owner_run_id: &str,
) -> Option<&'a str> {
    let entry = history.iter().rev().find(|entry| {
        entry.by == "system"
            && matches!(
                entry.event.as_str(),
                "resume_readmitted" | "resume_review_restored"
            )
    })?;
    let note = entry.note.as_deref()?;
    let (note, owner) = note.rsplit_once("; owner_run_id=")?;
    if owner != owner_run_id {
        return None;
    }
    note.strip_prefix("resume lineage reconciliation: run '")?
        .split_once("' resumes '")
        .map(|(run, _)| run)
}
