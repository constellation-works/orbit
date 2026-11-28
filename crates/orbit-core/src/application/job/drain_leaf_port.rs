//! The auto drain's leaf settlement a reconciling run read performs
//! [ORB-23198].
//!
//! A detached `task_auto_pipeline` leaf can end after its drain's last
//! admission pass, and the child run does not record its parent, so a
//! reconciling read of the drain is what records it terminal on the
//! dispatch. `workspace_auto` owns that record and also drives job runs, so a
//! direct call from here would close a cycle. This port is the way in:
//! `unit::workspace_auto` implements it on its runtime-backed `DrainLeafPort`,
//! bound in `app::port`.

use orbit_types::workflow::JobRun;

/// Drain leaf settlement a reconciling run read calls, implemented by
/// `unit::workspace_auto`.
pub(crate) trait JobDrainLeafPort {
    /// When `run` is an auto drain, record each of its open leaf dispatches
    /// whose child run has ended as terminal with the child's final status.
    /// Best-effort: a store failure is logged, and the next read or pass
    /// retries.
    fn record_finished_drain_leaves(&self, run: &JobRun);
}
