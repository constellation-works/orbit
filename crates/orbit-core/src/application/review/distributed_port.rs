//! What the review switches need from the distributed owner [ORB-17939].
//!
//! The before-landing hosts switch warns when the owner ships locally or
//! lists itself. `distributed` resolves the owner's ship contract and
//! machine, and it also drives review, so a direct call from here would
//! close a cycle. This port is the way in: `distributed` implements it on
//! [`OrbitRuntime`](crate::OrbitRuntime), so composing the runtime binds it.
//! The handoff console's commit boundary and completion authority come
//! through [`JobDrainPort`](crate::application::job::JobDrainPort), which
//! already carries both.

use orbit_store::contracts::AdmissionShipContract;

/// The owner reads review code calls, implemented by `distributed`.
pub(in crate::application) trait ReviewDistributedPort {
    /// Ship configuration as the owner resolves it for its own deliveries.
    fn owner_delivery_ship(&self) -> AdmissionShipContract;

    /// The machine that owns this workspace's distributed drain: the
    /// workspace owner, else this host's automation identity.
    fn drain_owner_machine(&self) -> Option<String>;
}
