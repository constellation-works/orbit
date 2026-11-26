//! Host-layer accessors on [`RuntimeKernel`]: execution assets and host-scoped budgets and caches.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::RuntimeKernel;

impl RuntimeKernel {
    /// The shared base-run budget of one owner claim settlement.
    pub fn claim_known_red_budget(&self) -> std::time::Duration {
        self.host().claim_known_red_budget
    }

    /// The last tree each run passed full required validation on, by run id.
    /// Held in this process and shared by clones.
    pub fn validated_trees(&self) -> &Arc<Mutex<BTreeMap<String, orbit_engine::ValidatedTree>>> {
        &self.host().validated_trees
    }

    /// When each pull request was last checked for an out-of-run repair, by
    /// pull request number. Held in this process and shared by clones.
    pub fn pr_repair_checks(&self) -> &Arc<Mutex<BTreeMap<u64, std::time::Instant>>> {
        &self.host().pr_repair_checks
    }

    pub fn tool_registry(&self) -> &orbit_tools::ToolRegistry {
        self.host().registry()
    }

    /// What the host plugin load pass registered and refused when this
    /// runtime was built.
    pub fn plugin_load(&self) -> &super::plugin::host::PluginHostLoad {
        self.host().plugin_load()
    }

    pub fn skill_catalog(&self) -> &orbit_store::FileStore<orbit_store::skill_store::SkillDirs> {
        self.host().skill_catalog()
    }
}
