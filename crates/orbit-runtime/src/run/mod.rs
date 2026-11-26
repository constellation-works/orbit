//! [`Run`]: [`Host`] plus the run this process carries — the owner transport
//! it coordinates through and where automation executes.

pub mod cost;
pub mod engine;
mod identity;
pub mod recovery_decision;
pub mod run_input;
pub mod task_coupling;
pub mod upgrade_handover;
pub(crate) mod worker_coordination;

use std::sync::Arc;

use crate::base::Base;
use crate::host::Host;
use crate::stored::Stored;

/// The layer code inside a drain or a worker runs against.
#[derive(Clone)]
pub struct Run {
    /// A follower drain's owner transport, supplied by the registry-owning
    /// composition layer that holds the federated destinations [ORB-13625].
    /// Absent on a standalone runtime, whose pull drain then refuses rather
    /// than reaching for a local store.
    pub(crate) drain_owner_transport: Option<Arc<dyn orbit_tools::DrainOwnerTransport>>,
    pub(crate) automation_execution_location: Option<Arc<orbit_types::task::ExecutionLocation>>,
    // Declared last so what the run holds is dropped before the host below.
    host: Host,
}

impl Run {
    /// A run as a runtime opens it, with nothing yet supplied by composition.
    pub(crate) fn new(host: Host) -> Self {
        Self {
            drain_owner_transport: None,
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
