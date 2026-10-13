//! The workspace auto-drain's deterministic actions: the per-iteration
//! admission classifier, the `--approve-proposed` selection, pilot wait and
//! report, the drain window, and the read-only readiness projection, plus the live drain
//! and leaf-run reads they share.

mod approvals;
mod classify;
mod drains;
mod readiness;
mod window;

pub(crate) use approvals::{
    await_approval_pilot, record_proposed_approvals, select_proposed_approvals,
};
pub(crate) use classify::classify_workspace_auto_tasks;
pub(super) use drains::read_live_leaf_runs;
pub(crate) use window::drain_window;
