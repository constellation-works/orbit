//! Owner repair of references omitted by older follower handoffs.

use orbit_common::OrbitError;
use orbit_types::task::TaskStatus;

use crate::OrbitRuntime;
use crate::application::job::JobDrainPort;

impl OrbitRuntime {
    /// Restore missing PR references on this owner's review tasks from accepted
    /// follower handoffs, without changing claim or landing authority.
    pub fn repair_missing_handoff_pr_refs(&self) -> Result<Vec<String>, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let boundary = self.owner_commit_boundary()?;
        let mut repaired = Vec::new();
        for task in
            self.list_tasks_filtered(Some(TaskStatus::Review), None, None, None, None, None)?
        {
            if boundary.repair_missing_handoff_pr_ref(&task.id)? {
                repaired.push(task.id);
            }
        }
        Ok(repaired)
    }
}
