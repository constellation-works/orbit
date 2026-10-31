//! The auto-drain concurrency mode job submission captures [ORB-17938].
//!
//! Every submission and replay of the auto drain records the concurrency
//! mode it runs under. `workspace_auto` owns that policy, and it also drives
//! job runs, so a direct call from here would close a cycle. This port is
//! the way in: `workspace_auto` implements it on
//! [`OrbitRuntime`](crate::OrbitRuntime), so composing the runtime binds it.

use orbit_common::OrbitError;
use serde_json::Value;

/// Auto-concurrency capture job submission calls, implemented by
/// `workspace_auto`.
pub(in crate::application) trait JobAutoConcurrencyPort {
    /// Record in `input` the concurrency mode a submission of `job_name`
    /// runs under, or refuse an override the job does not support.
    /// `retain_snapshot` keeps the provenance a resumed or replayed run
    /// already carries.
    fn install_auto_concurrency(
        &self,
        job_name: &str,
        input: &mut Value,
        retain_snapshot: bool,
    ) -> Result<(), OrbitError>;
}
