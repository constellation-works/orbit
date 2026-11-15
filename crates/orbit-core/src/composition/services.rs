//! Host services composed from global configuration and host stores.

use std::path::Path;

use orbit_common::OrbitError;

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
