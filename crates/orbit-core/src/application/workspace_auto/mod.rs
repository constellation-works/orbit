//! The workspace auto-drain's deterministic actions: the per-iteration
//! admission classifier, the `--approve-proposed` selection, pilot wait and
//! report, the drain window, the read-only readiness projection and limiter
//! shares, plus the live drain and leaf-run reads they share.

mod adopt;
mod approvals;
mod classify;
mod drains;
mod job_port;
mod limiters;
mod mode;
mod readiness;
mod task_port;
mod window;

pub(crate) use adopt::adopt_orphaned_deliveries;
pub(crate) use approvals::{
    await_approval_pilot, record_proposed_approvals, select_proposed_approvals,
};
pub(crate) use classify::classify_workspace_auto_tasks;
#[cfg(feature = "test-support")]
pub use classify::release_failure_hook::with_reservation_release_failure;
pub(crate) use window::drain_window;
