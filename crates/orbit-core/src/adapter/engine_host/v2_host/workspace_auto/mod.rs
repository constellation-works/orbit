//! The workspace auto-drain's deterministic actions: the per-iteration
//! admission classifier, the `--approve-proposed` selection and report, the
//! drain window, and the read-only readiness projection, plus the live drain
//! and leaf-run reads they share.

use orbit_engine::DispatchError;

mod approvals;
mod classify;
mod drains;
mod readiness;
mod window;

pub(super) use approvals::{record_proposed_approvals, select_proposed_approvals};
pub(super) use classify::classify_workspace_auto_tasks;
pub(super) use drains::read_live_leaf_runs;
pub(super) use window::drain_window;

fn action_failed(action: &str, message: String) -> DispatchError {
    DispatchError::DeterministicActionFailed {
        action: action.to_string(),
        message,
    }
}
