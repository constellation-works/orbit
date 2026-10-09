use std::path::Path;

use orbit_common::OrbitError;

use super::resolve::{
    read_optional, resolve_machine_id, resolve_machine_name, resolve_task_prefix,
};
use crate::memory_limit::MemoryLimit;

/// Resource limits for each detached pipeline worker (`machine.worker_*`).
///
/// Values are already admitted: memory limits are typed [`MemoryLimit`]s, so
/// a consumer only formats them for the service manager and has nothing left
/// to reject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerContainmentSettings {
    /// `machine.worker_containment` — launch workers in their own scope.
    pub enabled: bool,
    /// `machine.worker_containment_strict` — refuse uncontained launches.
    pub strict: bool,
    /// `machine.worker_memory_high` — throttling threshold.
    pub memory_high: MemoryLimit,
    /// `machine.worker_memory_max` — hard limit; OOM kills stay inside the run.
    pub memory_max: MemoryLimit,
    /// `machine.worker_tasks_max` — process/thread ceiling.
    pub tasks_max: u32,
}

/// The `[machine]` table, admitted on its own.
///
/// Identity is resolved on every runtime open and by `orbit init` before the
/// rest of the document is known to admit, so it is readable without resolving
/// crews, execution policy, or review preferences. The values still go
/// through the registry rows' own resolvers, so there is exactly one validator
/// and `orbit config get machine.id` cannot disagree with a runtime open.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MachineSettings {
    /// `machine.id` — the stable generated `hm_…` identity.
    pub id: Option<String>,
    /// `machine.name` — the operator-chosen display name.
    pub name: Option<String>,
    /// `machine.task_prefix` — the immutable task-id namespace.
    pub task_prefix: Option<String>,
}

impl MachineSettings {
    /// Admit `[machine]` from one already-parsed document.
    pub(crate) fn admit(document: &toml::Value, config_path: &Path) -> Result<Self, OrbitError> {
        let settings = Self {
            id: resolve_machine_id(read_optional(document, "machine.id", config_path)?)?,
            name: resolve_machine_name(read_optional(document, "machine.name", config_path)?)?,
            task_prefix: resolve_task_prefix(read_optional(
                document,
                "machine.task_prefix",
                config_path,
            )?)?,
        };
        settings.check_complete()?;
        Ok(settings)
    }

    /// The complete identity, or `None` when no `[machine]` table exists.
    /// A partial table never reaches here — `check_complete` refuses it.
    pub fn complete(self) -> Option<(String, String, String)> {
        Some((self.id?, self.name?, self.task_prefix?))
    }

    /// `[machine]` is one identity, not three independent settings: a file
    /// either carries the whole table or none of it. A partial table is a hand
    /// edit that would otherwise resolve to a machine with no id or no
    /// namespace, so it fails closed naming the missing keys.
    pub(super) fn check_complete(&self) -> Result<(), OrbitError> {
        let present = [
            ("machine.id", self.id.is_some()),
            ("machine.name", self.name.is_some()),
            ("machine.task_prefix", self.task_prefix.is_some()),
        ];
        if present.iter().all(|(_, set)| *set) || present.iter().all(|(_, set)| !*set) {
            return Ok(());
        }
        let missing = present
            .iter()
            .filter(|(_, set)| !*set)
            .map(|(key, _)| *key)
            .collect::<Vec<_>>()
            .join(", ");
        Err(OrbitError::InvalidInput(format!(
            "[machine] is incomplete: {missing} must be set alongside the keys already present; \
             run `orbit init` to create this machine's identity"
        )))
    }
}

/// High-water and recovery thresholds for the host resource verdict.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ResourceThrottleSettings {
    /// Evaluate a throttle verdict while always retaining pressure telemetry.
    pub enabled: bool,
    /// CPU high-water percentage.
    pub cpu_high_percent: u8,
    /// CPU recovery percentage, strictly below the high-water mark.
    pub cpu_resume_percent: u8,
    /// Leaves CPU-light work may hold while only CPU pressure throttles.
    pub cpu_light_leaves: u8,
    /// Memory high-water percentage.
    pub memory_high_percent: u8,
    /// Memory recovery percentage.
    pub memory_resume_percent: u8,
    /// Disk high-water percentage.
    pub disk_high_percent: u8,
    /// Disk recovery percentage.
    pub disk_resume_percent: u8,
}
