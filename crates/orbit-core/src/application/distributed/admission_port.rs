//! What pull admission needs from the executor census [ORB-17939].
//!
//! The owner orders the backlog a pull is admitted from against the live
//! executor census, with the requester declared by its request. `admission`
//! owns the census, and it also drives the distributed drain, so a direct
//! call from here would close a cycle. This port is the way in: `admission`
//! implements it on [`OrbitRuntime`](crate::OrbitRuntime), so composing the
//! runtime binds it.

use orbit_store::contracts::{ExecutorCapability, ExecutorCensus};

/// The census read distributed code calls, implemented by `admission`.
pub(in crate::application) trait DistributedAdmissionPort {
    /// The live census `requester` orders against, with its own entry
    /// replaced by `capability`, or `None` when ordering is off or the
    /// census cannot be read.
    fn pull_requester_census(
        &self,
        requester: &str,
        capability: ExecutorCapability,
    ) -> Option<ExecutorCensus>;
}
