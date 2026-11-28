//! What condition evaluation needs from the drain [ORB-19990].
//!
//! `board.eligible` is the drain's own admission view and
//! `board.drain_running` its live coordinator. `workspace_auto` owns both and
//! sits above automation, so a direct call from here would be an upward
//! edge. This port is the way in: `unit::workspace_auto` implements it on its
//! runtime-backed `AutomationPort`, bound in `app::port`.

use orbit_common::OrbitError;

/// Drain reads condition explain makes, implemented by `unit::workspace_auto`.
pub(crate) trait AutomationBoardPort {
    /// Backlog tasks the drain's admission view would admit now, observed
    /// without its commit boundary: scheduled, held and excluded tasks are
    /// not listed.
    fn eligible_task_ids(&self) -> Result<Vec<String>, OrbitError>;

    /// Whether this workspace's local drain is running.
    fn local_drain_running(&self) -> Result<bool, OrbitError>;
}
