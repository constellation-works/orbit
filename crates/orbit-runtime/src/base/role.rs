//! The process's role, resolved once when [`Base`](super::Base) is built.

use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_types::tool::WorkerInvocation;
use orbit_types::workspace::WorkspaceCheckoutRole;

use super::binding::WorkspaceRuntimeBinding;
use crate::RuntimeKernel;

/// What kind of process this runtime serves, resolved once when [`Base`](super::Base)
/// is built from the workspace binding and the worker invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    /// An operator's command against an unbound or standalone checkout.
    Operator,
    /// A registered owner checkout that serves local commands and drains.
    DrainOwner,
    /// A claimed managed worker, carrying its authenticated invocation.
    Worker(Arc<WorkerInvocation>),
    /// A replica checkout whose coordination state belongs to another machine.
    Replica {
        /// Stable machine id of the host that owns this workspace.
        owner_machine_id: Arc<str>,
    },
}

impl Role {
    /// Resolve the process's role once from its workspace binding and worker
    /// invocation.
    pub fn resolve(
        binding: Option<&WorkspaceRuntimeBinding>,
        worker_invocation: Option<Arc<WorkerInvocation>>,
    ) -> Self {
        Self::resolve_inputs(binding, worker_invocation, None)
    }

    /// Re-resolve through [`Self::resolve_inputs`] when a compatibility builder
    /// supplies or clears an explicit replica owner.
    pub(crate) fn with_replica_owner_override(
        &self,
        binding: Option<&WorkspaceRuntimeBinding>,
        owner_machine_id: Option<String>,
    ) -> Self {
        let worker_invocation = match self {
            Self::Worker(invocation) => Some(Arc::clone(invocation)),
            Self::Operator | Self::DrainOwner | Self::Replica { .. } => None,
        };
        Self::resolve_inputs(binding, worker_invocation, Some(owner_machine_id))
    }

    /// The single function that computes [`Role`] from the workspace binding,
    /// the worker invocation, and any explicit replica-owner override.
    fn resolve_inputs(
        binding: Option<&WorkspaceRuntimeBinding>,
        worker_invocation: Option<Arc<WorkerInvocation>>,
        replica_owner_override: Option<Option<String>>,
    ) -> Self {
        if let Some(invocation) = worker_invocation {
            return Self::Worker(invocation);
        }
        match replica_owner_override {
            Some(Some(owner_machine_id)) => Self::Replica {
                owner_machine_id: Arc::from(owner_machine_id),
            },
            Some(None) => match binding.and_then(|binding| binding.checkout_role) {
                Some(WorkspaceCheckoutRole::Owner) => Self::DrainOwner,
                _ => Self::Operator,
            },
            None => match binding {
                Some(binding) if binding.checkout_role == Some(WorkspaceCheckoutRole::Replica) => {
                    match binding.owner_machine_id.as_deref() {
                        Some(owner_machine_id) => Self::Replica {
                            owner_machine_id: Arc::from(owner_machine_id),
                        },
                        None => Self::Operator,
                    }
                }
                Some(binding) if binding.checkout_role == Some(WorkspaceCheckoutRole::Owner) => {
                    Self::DrainOwner
                }
                _ => Self::Operator,
            },
        }
    }

    /// The bound worker invocation when this process is a claimed worker.
    pub fn invocation(&self) -> Option<&WorkerInvocation> {
        match self {
            Self::Worker(invocation) => Some(invocation.as_ref()),
            Self::Operator | Self::DrainOwner | Self::Replica { .. } => None,
        }
    }

    /// Whether this process serves a replica checkout.
    pub fn is_replica(&self) -> bool {
        matches!(self, Self::Replica { .. })
    }

    /// The owning machine id when this process runs against a replica checkout.
    pub fn replica_owner(&self) -> Option<&str> {
        match self {
            Self::Replica { owner_machine_id } => Some(owner_machine_id.as_ref()),
            Self::Operator | Self::DrainOwner | Self::Worker(_) => None,
        }
    }

    /// Bind the worker's owner route, or select this checkout's local access.
    pub(crate) fn coordination<W>(
        &self,
        bind_worker: impl FnOnce(&Arc<WorkerInvocation>) -> W,
    ) -> CoordinationAccess<W> {
        match self {
            Self::Operator | Self::DrainOwner => CoordinationAccess::Local,
            Self::Worker(invocation) => CoordinationAccess::Claimed(bind_worker(invocation)),
            Self::Replica { owner_machine_id } => {
                CoordinationAccess::Replica(owner_machine_id.clone())
            }
        }
    }

    /// Whether coordination task projections are visible for this role.
    pub fn task_reads_visible(&self) -> bool {
        !self.is_replica()
    }

    /// Why a local coordination write is unavailable for this role, if it is.
    pub fn task_write_refusal(&self) -> Option<OrbitError> {
        self.coordination(|_| ()).ensure_write().err()
    }
}

/// Access to task coordination state. A claimed worker carries its bound owner
/// route. A replica carries the id used in its write refusal.
#[derive(Clone)]
pub(crate) enum CoordinationAccess<W> {
    Local,
    Claimed(W),
    Replica(Arc<str>),
}

impl<W> CoordinationAccess<W> {
    pub(crate) fn reads_visible(&self) -> bool {
        !matches!(self, Self::Replica(_))
    }

    pub(crate) fn ensure_write(&self) -> Result<(), OrbitError> {
        match self {
            Self::Local => Ok(()),
            Self::Claimed(_) => Err(OrbitError::PolicyDenied(
                "claimed coordination writes require the owner route".into(),
            )),
            Self::Replica(owner_machine_id) => Err(OrbitError::CapabilityRefused(format!(
                "control_plane coordination writes are refused in this replica checkout; workspace is owned by machine '{owner_machine_id}'"
            ))),
        }
    }
}

impl RuntimeKernel {
    /// The process's resolved role.
    pub fn role(&self) -> &Role {
        self.base().role()
    }

    /// Attach or clear the declared remote owner for a replica checkout.
    /// Supplied by the registry-owning composition layer or fixtures; never
    /// inferred by Core.
    pub fn with_coordination_write_owner(mut self, owner_machine_id: Option<String>) -> Self {
        let role = self.base().role().with_replica_owner_override(
            self.base().workspace_binding.as_deref(),
            owner_machine_id,
        );
        self.stored_mut().set_role(role);
        self
    }

    pub fn worker_invocation(&self) -> Option<&WorkerInvocation> {
        self.base().role().invocation()
    }

    /// The owner machine id when this runtime is a replica checkout, so a
    /// surface can tell that owner-only drain controls are not offered here.
    pub fn replica_owner_machine(&self) -> Option<&str> {
        self.base().role().replica_owner()
    }
}
