//! The operator's one-shot backfill of dispatch effort for invocations that
//! completed before Orbit recorded a dispatch identity. Never run on open.

use orbit_common::OrbitError;
use orbit_store::workflow::dispatch_effort;

pub use orbit_store::workflow::dispatch_effort::DispatchEffortBackfill;

use crate::OrbitRuntime;

impl OrbitRuntime {
    /// Record a dispatch identity, with the effort its launch argv rendered,
    /// for every invocation of this workspace that has none, marked as
    /// backfilled. The crew is the run's resolved crew only for the run's
    /// executor invocation. Invocations with no matching launch, or whose
    /// matching launches disagree on effort, stay unknown. A run whose
    /// invocation never completed gets one launch record per audited launch
    /// that one step contains and that names a provider, and a launch a
    /// dispatch-time record already covers is left as that record. Running it
    /// again changes nothing.
    pub fn backfill_dispatch_effort(&self) -> Result<DispatchEffortBackfill, OrbitError> {
        let workspace_id = self.stores().jobs().workspace_id().to_string();
        dispatch_effort::backfill_dispatch_effort(
            &self.sqlite_store()?,
            &workspace_id,
            orbit_runtime::cost::IMPLEMENT_INVOCATION_IDS,
        )
    }
}
