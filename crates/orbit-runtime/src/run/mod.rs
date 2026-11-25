//! [`Run`]: [`Host`] plus the run this process carries — the worker
//! invocation, the owner it coordinates through and where automation executes.

use std::sync::Arc;

use crate::base::Base;
use crate::host::Host;
use crate::stored::Stored;

/// The layer code inside a drain or a worker runs against.
#[derive(Clone)]
pub struct Run {
    pub(crate) worker_invocation: Option<Arc<orbit_types::tool::WorkerInvocation>>,
    pub(crate) owner_coordinator: Option<Arc<dyn orbit_tools::OwnerCoordinator>>,
    /// A follower drain's owner transport, supplied by the registry-owning
    /// composition layer that holds the federated destinations [ORB-13625].
    /// Absent on a standalone runtime, whose pull drain then refuses rather
    /// than reaching for a local store.
    pub(crate) drain_owner_transport: Option<Arc<dyn orbit_tools::DrainOwnerTransport>>,
    /// A higher-level registry may mark this local checkout as a replica. The
    /// runtime stays registry-neutral; it only carries the refusal supplied by
    /// that owner so every task-record writer shares one fail-closed gate.
    pub(crate) coordination_write_owner: Option<Arc<str>>,
    pub(crate) automation_execution_location: Option<Arc<orbit_types::task::ExecutionLocation>>,
    // Declared last so what the run holds is dropped before the host below.
    host: Host,
}

impl Run {
    /// A run as a runtime opens it: bound to `worker_invocation` when the
    /// process restored one, with nothing yet supplied by composition.
    pub(crate) fn new(
        host: Host,
        worker_invocation: Option<Arc<orbit_types::tool::WorkerInvocation>>,
    ) -> Self {
        Self {
            worker_invocation,
            owner_coordinator: None,
            drain_owner_transport: None,
            coordination_write_owner: None,
            automation_execution_location: None,
            host,
        }
    }

    /// The layer below.
    pub fn host(&self) -> &Host {
        &self.host
    }

    /// The stores layer.
    pub fn stored(&self) -> &Stored {
        self.host.stored()
    }

    /// The lowest layer.
    pub fn base(&self) -> &Base {
        self.host.base()
    }

    pub(crate) fn host_mut(&mut self) -> &mut Host {
        &mut self.host
    }
}
