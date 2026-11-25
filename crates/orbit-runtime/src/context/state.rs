use super::{OrbitExecutionAssets, OrbitPolicyContext, OrbitRuntimeSettings, OrbitStores};
use orbit_types::workspace::WorkspacePaths;

/// What the builder assembles for one open, before the kernel sorts each part
/// into the runtime layer that holds it: the paths, the policy and the
/// settings into `Base`, the stores into `Stored`, the execution assets into
/// `Host`.
pub(crate) struct OrbitContext {
    pub(crate) paths: WorkspacePaths,
    pub(crate) stores: OrbitStores,
    pub(crate) execution: OrbitExecutionAssets,
    pub(crate) policy: OrbitPolicyContext,
    pub(crate) settings: OrbitRuntimeSettings,
}

impl OrbitContext {
    pub(crate) fn new(
        paths: WorkspacePaths,
        stores: OrbitStores,
        execution: OrbitExecutionAssets,
        policy: OrbitPolicyContext,
        settings: OrbitRuntimeSettings,
    ) -> Self {
        Self {
            paths,
            stores,
            execution,
            policy,
            settings,
        }
    }
}
