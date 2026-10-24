//! What external-merge settlement needs from the workspace auto drain
//! [ORB-17936].
//!
//! A task a live `run auto` leaf carries is not settled from outside, and
//! a leaf hands a task to its child before the task records the child's
//! run. `workspace_auto` owns the leaf runs, and it also drives task writes,
//! so a direct call from here would close a cycle. This port is the way in:
//! `workspace_auto` implements it on [`OrbitRuntime`](crate::OrbitRuntime),
//! so composing the runtime binds it.

use std::collections::BTreeSet;

use orbit_common::OrbitError;

/// The drain read task code calls, implemented by `workspace_auto`.
pub(in crate::application) trait TaskWorkspaceAutoPort {
    /// The IDs of the tasks live `run auto` leaf runs carry.
    fn live_leaf_task_ids(&self) -> Result<BTreeSet<String>, OrbitError>;
}
