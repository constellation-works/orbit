//! The tasks coupled to a run: reservation cleanup when the run reaches a
//! terminal state, blocking or requeueing its tasks, and provider holds.

mod block_on_run_failure;
pub mod provider_hold;
mod reservation_cleanup;

pub use block_on_run_failure::resumed_task_run_id;
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
