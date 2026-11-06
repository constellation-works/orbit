//! Host services composed from global configuration and host stores.

use std::path::Path;
use std::sync::Arc;

use orbit_common::OrbitError;

use crate::runtime::OrbitRuntime;

/// Compose a serving-host resource monitor from global configuration only,
/// without opening or repairing a workspace runtime.
pub fn host_resource_monitor(
    global_root: &Path,
) -> Result<crate::runtime::host_resource::HostResourceMonitor, OrbitError> {
    let snapshot = orbit_config::ConfigStore::open(
        orbit_config::ConfigScope::Global,
        global_root.join("config.toml"),
    )?
    .snapshot()?;
    Ok(crate::runtime::host_resource::HostResourceMonitor::new(
        crate::runtime::host_resource::default_host_resource_probe(),
        snapshot.resource_throttle(),
    )
    .with_shared_history(global_root))
}

/// Compose optional assistance from host-only settings and the host store.
/// No worker or consumer is automatically started. The loader reopens global
/// configuration at each boundary so an operator's off switch takes effect.
pub fn host_assistance_service(
    global_root: &Path,
    store: Arc<dyn orbit_store::contracts::assistance::AssistanceStoreBackend>,
) -> crate::application::assistance::AssistanceService {
    let root = global_root.to_path_buf();
    crate::application::assistance::AssistanceService::new(
        store,
        Arc::new(crate::adapter::decisions::OpenAiDecisionsAdapter::default()),
        Arc::new(move || {
            Ok(orbit_config::ConfigStore::open(
                orbit_config::ConfigScope::Global,
                root.join("config.toml"),
            )?
            .snapshot()?
            .assistance_settings)
        }),
    )
}

impl crate::application::assistance::AssistanceServicePort for OrbitRuntime {
    /// Join an assistance consumer to host-only policy, the adapter and the ledger.
    fn assistance_service(
        &self,
    ) -> Result<crate::application::assistance::AssistanceService, OrbitError> {
        Ok(host_assistance_service(
            &self.global_root(),
            orbit_store::compose::assistance_store_from_store(self.sqlite_store()?),
        ))
    }
}
