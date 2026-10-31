use orbit_common::OrbitError;
use orbit_types::plugin::InstalledPlugin;
use orbit_types::policy::PolicyDef;
use orbit_types::tool::StoredTool;
use orbit_types::workflow::ExecutorDef;

pub trait ToolStoreBackend: Send + Sync {
    fn list_tools(&self) -> Result<Vec<StoredTool>, OrbitError>;
    fn get_tool(&self, name: &str) -> Result<Option<StoredTool>, OrbitError>;
    fn insert_tool(&self, tool: &StoredTool) -> Result<(), OrbitError>;
    fn delete_tool(&self, name: &str) -> Result<bool, OrbitError>;
    fn set_tool_enabled(&self, name: &str, enabled: bool) -> Result<bool, OrbitError>;
}

/// Host-local plugin records: what is installed, where, and whether it is
/// enabled. Never synced; the workspace pin file is the versioned half.
pub trait PluginStoreBackend: Send + Sync {
    fn list_plugins(&self) -> Result<Vec<InstalledPlugin>, OrbitError>;
    fn get_plugin(&self, name: &str) -> Result<Option<InstalledPlugin>, OrbitError>;
    fn upsert_plugin(&self, plugin: &InstalledPlugin) -> Result<(), OrbitError>;
    fn delete_plugin(&self, name: &str) -> Result<bool, OrbitError>;
    /// Record the Orbit version whose conformance run this plugin passed.
    fn set_plugin_certification(
        &self,
        name: &str,
        orbit_version: Option<&str>,
    ) -> Result<bool, OrbitError>;
    fn set_plugin_enabled(
        &self,
        name: &str,
        enabled: bool,
        grants: &[String],
    ) -> Result<bool, OrbitError>;
}

pub trait ExecutorDefStoreBackend: Send + Sync {
    fn list_executor_defs(&self) -> Result<Vec<ExecutorDef>, OrbitError>;
    fn get_executor_def(&self, name: &str) -> Result<Option<ExecutorDef>, OrbitError>;
    fn upsert_executor_def(&self, def: &ExecutorDef) -> Result<(), OrbitError>;
    /// Remove one executor definition by name. `Ok(false)` when absent.
    fn delete_executor_def(&self, _name: &str) -> Result<bool, OrbitError> {
        Ok(false)
    }
}

pub trait PolicyDefStoreBackend: Send + Sync {
    fn list_policy_defs(&self) -> Result<Vec<PolicyDef>, OrbitError>;
    fn get_policy_def(&self, name: &str) -> Result<Option<PolicyDef>, OrbitError>;
    fn upsert_policy_def(&self, def: &PolicyDef) -> Result<(), OrbitError>;
}
