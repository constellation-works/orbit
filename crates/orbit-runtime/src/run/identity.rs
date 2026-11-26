//! Run-layer execution identity and coordination ownership on [`RuntimeKernel`].

use std::sync::Arc;

use orbit_common::OrbitError;

use crate::RuntimeKernel;

impl RuntimeKernel {
    /// Registry-owning composition supplies stable machine identity; Core never discovers it.
    pub fn with_automation_machine_identity(self, machine_id: Option<String>) -> Self {
        self.with_automation_execution_location(machine_id.map(|machine_id| {
            orbit_types::task::ExecutionLocation {
                machine_id,
                machine_name: None,
            }
        }))
    }

    /// Supply the executing machine's stable identity and configured display name.
    pub fn with_automation_execution_location(
        mut self,
        location: Option<orbit_types::task::ExecutionLocation>,
    ) -> Self {
        self.stored_mut().set_execution_location(location.clone());
        self.run_mut().automation_execution_location = location.map(Arc::new);
        self
    }

    pub fn automation_machine_identity(&self) -> Option<&str> {
        self.automation_execution_location()
            .map(|location| location.machine_id.as_str())
    }

    pub fn automation_execution_location(&self) -> Option<&orbit_types::task::ExecutionLocation> {
        self.run().automation_execution_location.as_deref()
    }

    /// Attach the declared remote owner for a replica checkout. This is set by
    /// the registry-owning composition layer, never inferred by Core.
    pub fn with_coordination_write_owner(mut self, owner_machine_id: Option<String>) -> Self {
        self.run_mut().coordination_write_owner = owner_machine_id.map(Arc::from);
        self
    }

    /// Why coordination writes are unavailable from this runtime, if they are.
    ///
    /// Replica checkouts name the owner in their capability refusal. Claimed
    /// workers are refused because their writes must use the owner route.
    pub fn coordination_task_write_refusal(&self) -> Option<OrbitError> {
        if self.worker_invocation().is_some() {
            return Some(OrbitError::PolicyDenied(
                "claimed coordination writes require the owner route".into(),
            ));
        }
        let owner_machine_id = self.run().coordination_write_owner.as_deref()?;
        Some(OrbitError::CapabilityRefused(format!(
            "control_plane coordination writes are refused in this replica checkout; workspace is owned by machine '{owner_machine_id}'"
        )))
    }

    /// Refuse control-plane work in a replica checkout.
    ///
    /// The refusal is a catalog-role capability outcome, not a malformed call:
    /// federated routing has to tell "this destination will not run that class"
    /// apart from "that request was invalid", so this reports
    /// `CapabilityRefused` [ORB-11012].
    pub fn ensure_coordination_task_write_permitted(&self) -> Result<(), OrbitError> {
        self.coordination_task_write_refusal().map_or(Ok(()), Err)
    }

    pub fn coordination_task_reads_visible(&self) -> bool {
        self.run().coordination_write_owner.is_none()
    }

    /// The remote owner declared for a replica checkout, if this is one.
    pub fn coordination_write_owner(&self) -> Option<&str> {
        self.run().coordination_write_owner.as_deref()
    }

    /// The owner machine id when this runtime is a replica checkout, so a
    /// surface can tell that owner-only drain controls are not offered here.
    pub fn replica_owner_machine(&self) -> Option<&str> {
        self.coordination_write_owner()
    }
}
