//! Plugin broker start and worker process registration at the engine boundary.

use orbit_common::OrbitError;
use orbit_engine::{PluginBrokerHandle, PluginBrokerRun};

use crate::OrbitRuntime;
#[cfg(target_os = "linux")]
use crate::runtime::recovery_authority::RecoveryAuthority;

pub(super) fn start_plugin_broker(
    runtime: &OrbitRuntime,
    run: &PluginBrokerRun,
) -> Result<Option<Box<dyn PluginBrokerHandle>>, OrbitError> {
    #[cfg(unix)]
    {
        let dispatch = std::sync::Arc::new(crate::adapter::command::RunDispatch::new(
            runtime.clone(),
            run.clone(),
        ));
        let broker = crate::runtime::plugin::broker::PluginBroker::start(
            &runtime.global_root(),
            &run.run_id,
            dispatch,
        )?;
        Ok(Some(Box::new(broker)))
    }
    #[cfg(not(unix))]
    {
        let _ = (runtime, run);
        Ok(None)
    }
}

pub(super) fn register_worker_pid_namespace(
    runtime: &OrbitRuntime,
    pid: u32,
) -> Result<(), OrbitError> {
    #[cfg(target_os = "linux")]
    if let Some(binding) = runtime.worker_invocation() {
        RecoveryAuthority::open(&runtime.global_root())?.bind_worker_namespace(pid, binding)?;
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (runtime, pid);
    Ok(())
}
