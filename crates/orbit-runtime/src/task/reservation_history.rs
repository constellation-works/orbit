//! Read-only history of the workspace's file reservations.

use orbit_common::OrbitError;
use orbit_store::contracts::TaskReservationRecord;

use crate::RuntimeKernel;

use super::locks::{workspace_orbit_dir, workspace_task_reservation_id};

impl RuntimeKernel {
    /// Every file reservation this workspace recorded, active or released,
    /// oldest first. Released rows are kept as history, so this is the
    /// reservation record that aggregates such as the dashboard's change
    /// coupling read. It never expires or releases a row.
    pub fn list_task_reservation_history(&self) -> Result<Vec<TaskReservationRecord>, OrbitError> {
        self.stores()
            .task_reservations()
            .list_task_reservation_history(
                &workspace_orbit_dir(self),
                workspace_task_reservation_id(self)?.as_deref(),
            )
    }
}
