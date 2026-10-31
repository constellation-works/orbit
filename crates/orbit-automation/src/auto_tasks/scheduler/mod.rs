//! Auto-task scheduling policy with an explicit clock and Core lifecycle adapter.

mod dispatch;
mod faults;
mod fire;
mod locked;
mod preconditions;
mod report;
#[cfg(test)]
mod tests;

pub use dispatch::{AutoTaskDispatch, BlockedCandidateProbe, ChangeProbe, InactivePluginSkip};
pub use fire::run_auto_task_scheduler_at;
pub use preconditions::{NO_BLOCKED_CANDIDATES, UNCHANGED_SINCE_LAST_SWEEP};
pub use report::{AutoTaskFireReport, AutoTaskSchedulerOutcome, SchedulerOptions};

#[cfg(test)]
pub(crate) use faults::{
    SchedulerFault, inject_scheduler_fault, set_admission_overlap_barrier, set_after_load_barriers,
};
