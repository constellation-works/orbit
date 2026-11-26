//! Actor, workspace ownership, and composition identity on [`RuntimeKernel`].

use std::any::Any;
use std::sync::Arc;

use super::actor::ActorIdentity;
use crate::RuntimeKernel;

impl RuntimeKernel {
    pub fn with_actor(mut self, actor: ActorIdentity) -> Self {
        self.base_mut().set_actor(actor);
        self
    }

    /// Registered owner of the bound workspace, when composition supplied a
    /// binding that names one. Delivery automation resolves its default owner
    /// from this rather than from cwd or the running executor.
    pub fn workspace_owner_machine_id(&self) -> Option<&str> {
        self.base()
            .workspace_binding
            .as_ref()
            .and_then(|binding| binding.owner_machine_id.as_deref())
    }

    /// Attach data composition already loaded for this open.
    ///
    /// The kernel stores the snapshot and never reads it. Callers downcast to
    /// the concrete type they put in.
    pub fn with_composition_snapshot(mut self, snapshot: Arc<dyn Any + Send + Sync>) -> Self {
        self.base_mut().composition_snapshot = Some(snapshot);
        self
    }

    /// The snapshot [`Self::with_composition_snapshot`] stored, if any.
    pub fn composition_snapshot(&self) -> Option<&(dyn Any + Send + Sync)> {
        self.base().composition_snapshot.as_deref()
    }

    /// Record the registry identity of this runtime's checkout as the open
    /// resolved it, `None` being a recorded miss. Supplied by the
    /// registry-owning composition layer; Core never loads the registry to
    /// refill it.
    pub fn with_opened_registered_workspace(mut self, identity: Option<(String, String)>) -> Self {
        self.base_mut().opened_registered_workspace = Some(identity);
        self
    }

    /// The `(id, name)` [`Self::with_opened_registered_workspace`] recorded.
    /// The outer `None` means this open carried no registry binding; the inner
    /// `None` is a recorded miss.
    pub fn opened_registered_workspace(&self) -> Option<Option<(&str, &str)>> {
        self.base()
            .opened_registered_workspace
            .as_ref()
            .map(|identity| {
                identity
                    .as_ref()
                    .map(|(id, name)| (id.as_str(), name.as_str()))
            })
    }
}
