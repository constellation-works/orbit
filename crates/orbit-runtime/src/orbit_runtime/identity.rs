//! Actor, machine, workspace catalog, and coordination ownership.

use std::sync::Arc;

use orbit_common::OrbitError;

use super::RuntimeKernel;
use crate::context::ActorIdentity;
use crate::workspace::catalog;

impl RuntimeKernel {
    pub fn with_actor(mut self, actor: ActorIdentity) -> Self {
        self.context.set_actor(actor);
        self
    }

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
        self.context.set_execution_location(location.clone());
        self.automation_execution_location = location.map(Arc::new);
        self
    }

    pub fn automation_machine_identity(&self) -> Option<&str> {
        self.automation_execution_location()
            .map(|location| location.machine_id.as_str())
    }

    pub fn automation_execution_location(&self) -> Option<&orbit_types::task::ExecutionLocation> {
        self.automation_execution_location.as_deref()
    }

    /// Registered owner of the bound workspace, when composition supplied a
    /// binding that names one. Delivery automation resolves its default owner
    /// from this rather than from cwd or the running executor.
    pub fn workspace_owner_machine_id(&self) -> Option<&str> {
        self.workspace_binding
            .as_ref()
            .and_then(|binding| binding.owner_machine_id.as_deref())
    }

    /// Attach the declared remote owner for a replica checkout. This is set by
    /// the registry-owning composition layer, never inferred by Core.
    pub fn with_coordination_write_owner(mut self, owner_machine_id: Option<String>) -> Self {
        self.coordination_write_owner = owner_machine_id.map(Arc::from);
        self
    }

    /// Attach the workspace catalog that resolves federated search scope. Like
    /// the replica owner above, it is supplied by the registry-owning layer and
    /// never constructed by Core.
    pub fn with_workspace_catalog(mut self, catalog: Arc<dyn catalog::WorkspaceCatalog>) -> Self {
        self.workspace_catalog = Some(catalog);
        self
    }

    pub fn workspace_catalog(&self) -> Option<&Arc<dyn catalog::WorkspaceCatalog>> {
        self.workspace_catalog.as_ref()
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
        let owner_machine_id = self.coordination_write_owner.as_deref()?;
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
        self.coordination_write_owner.is_none()
    }

    /// The remote owner declared for a replica checkout, if this is one.
    pub fn coordination_write_owner(&self) -> Option<&str> {
        self.coordination_write_owner.as_deref()
    }

    /// The owner machine id when this runtime is a replica checkout, so a
    /// surface can tell that owner-only drain controls are not offered here.
    pub fn replica_owner_machine(&self) -> Option<&str> {
        self.coordination_write_owner()
    }
}
