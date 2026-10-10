use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::observability::log_rotation::LogRotationConfig;
use orbit_types::identity::Crew;
use orbit_types::workflow::automation::members::PreparationFreshness;

use super::machine::{MachineSettings, ResourceThrottleSettings, WorkerContainmentSettings};
use super::resolve::{DEFAULT_FINAL_RECOVERY_CREWS, resolve_default_crew};
use super::table::ConfigSnapshot;

impl ConfigSnapshot {
    pub(super) fn finish_admission(
        &mut self,
        crews: &BTreeMap<String, Crew>,
        env_default: Option<&str>,
        require_default_crew: bool,
    ) -> Result<(), OrbitError> {
        LogRotationConfig::from_parts(
            Some(self.runtime_log_retention_days),
            Some(self.runtime_log_max_total_mb),
            Some(self.runtime_log_max_file_mb),
        )?;
        admit_crew_pool(
            &mut self.workflow_low_complexity_crews,
            crews,
            "workflow.low_complexity_crews",
        )?;
        admit_crew_pool(
            &mut self.workflow_medium_complexity_crews,
            crews,
            "workflow.medium_complexity_crews",
        )?;
        admit_crew_pool(
            &mut self.workflow_hard_complexity_crews,
            crews,
            "workflow.hard_complexity_crews",
        )?;
        admit_crew_pool(
            &mut self.workflow_xhard_complexity_crews,
            crews,
            "workflow.xhard_complexity_crews",
        )?;
        self.workflow_final_recovery_crews = Some(admit_final_recovery_crews(
            self.workflow_final_recovery_crews.take(),
            crews,
        )?);
        self.workflow_default_crew = resolve_default_crew(
            self.workflow_default_crew.take(),
            crews,
            env_default,
            require_default_crew,
        )?;
        if self.machine_worker_containment_strict && !self.machine_worker_containment {
            return Err(OrbitError::InvalidInput(
                "machine.worker_containment_strict=true requires machine.worker_containment=true"
                    .to_string(),
            ));
        }
        let resources = self.resource_throttle();
        for (resource, high, resume) in [
            (
                "cpu",
                resources.cpu_high_percent,
                resources.cpu_resume_percent,
            ),
            (
                "memory",
                resources.memory_high_percent,
                resources.memory_resume_percent,
            ),
            (
                "disk",
                resources.disk_high_percent,
                resources.disk_resume_percent,
            ),
        ] {
            if resume >= high {
                return Err(OrbitError::InvalidInput(format!(
                    "workflow.resource_throttle.{resource}_resume_percent must be less than {resource}_high_percent"
                )));
            }
        }
        self.machine().check_complete()?;
        Ok(())
    }

    /// The admitted `workflow.final_recovery_crews` pool; empty disables final
    /// recovery. Admission always fills the field, so `None` never reaches here.
    pub fn final_recovery_crews(&self) -> &[String] {
        self.workflow_final_recovery_crews
            .as_deref()
            .unwrap_or_default()
    }

    /// This machine's identity, as admitted from the same registry rows every
    /// other consumer reads.
    pub fn machine(&self) -> MachineSettings {
        MachineSettings {
            id: self.machine_id.clone(),
            name: self.machine_name.clone(),
            task_prefix: self.machine_task_prefix.clone(),
        }
    }
}

impl ConfigSnapshot {
    /// The admitted `[workflow.task_pilot_freshness]` table: the global layer
    /// a routine's `trigger.state.freshness` overrides.
    pub fn task_pilot_freshness(&self) -> PreparationFreshness {
        PreparationFreshness {
            material_fields: self.workflow_task_pilot_freshness_material_fields.clone(),
            source_sensitivity: self.workflow_task_pilot_freshness_source_sensitivity,
        }
        .normalized()
    }

    /// The admitted `machine.worker_*` limits.
    pub fn worker_containment(&self) -> WorkerContainmentSettings {
        WorkerContainmentSettings {
            enabled: self.machine_worker_containment,
            strict: self.machine_worker_containment_strict,
            memory_high: self.machine_worker_memory_high,
            memory_max: self.machine_worker_memory_max,
            tasks_max: self.machine_worker_tasks_max,
            cpu_quota_percent: self.machine_worker_cpu_quota,
        }
    }
}

impl Default for ConfigSnapshot {
    fn default() -> Self {
        let document = toml::Value::Table(toml::map::Map::new());
        ConfigSnapshot::admit_with_env(
            &document,
            Path::new("<built-in defaults>"),
            &default_admission_crews(),
            None,
            true,
        )
        .unwrap_or_else(|error| panic!("built-in configuration defaults must admit: {error}"))
    }
}

/// The built-in crew registry a config without `[crews]` resolves to, so the
/// built-in snapshot admits the same defaults such a config does.
fn default_admission_crews() -> BTreeMap<String, Crew> {
    crate::resolved::default_crews()
}

/// Admit one `workflow.*_complexity_crews` value in place, replacing it with
/// its canonical `name[:weight]` rendering.
fn admit_crew_pool(
    pool: &mut Vec<String>,
    crews: &BTreeMap<String, Crew>,
    setting: &str,
) -> Result<(), OrbitError> {
    *pool = crate::canonical_crew_pool(pool, crews, setting)?.to_setting_value();
    Ok(())
}

/// Admit `workflow.final_recovery_crews`.
///
/// A written pool is admitted exactly like the complexity pools, so a typo
/// fails at load. An unset key is the built-in default, which names crews a
/// custom `[crews]` registry may not define; it keeps only the members that
/// registry does define rather than failing every command on such a host, and
/// a registry with none of them leaves final recovery disabled.
fn admit_final_recovery_crews(
    raw: Option<Vec<String>>,
    crews: &BTreeMap<String, Crew>,
) -> Result<Vec<String>, OrbitError> {
    let pool = match raw {
        Some(pool) => pool,
        None => DEFAULT_FINAL_RECOVERY_CREWS
            .iter()
            .filter(|entry| {
                let name = entry
                    .rsplit_once(crate::crew_pools::WEIGHT_SEPARATOR)
                    .map_or(**entry, |(name, _)| name);
                crews.contains_key(name)
            })
            .map(ToString::to_string)
            .collect(),
    };
    Ok(
        crate::canonical_crew_pool(&pool, crews, "workflow.final_recovery_crews")?
            .to_setting_value(),
    )
}

impl ConfigSnapshot {
    /// Admitted host resource thresholds, shared by dashboard and admission consumers.
    pub fn resource_throttle(&self) -> ResourceThrottleSettings {
        ResourceThrottleSettings {
            enabled: self.workflow_resource_throttle_enabled,
            cpu_high_percent: self.workflow_resource_throttle_cpu_high_percent,
            cpu_resume_percent: self.workflow_resource_throttle_cpu_resume_percent,
            cpu_light_leaves: self.workflow_resource_throttle_cpu_light_leaves,
            memory_high_percent: self.workflow_resource_throttle_memory_high_percent,
            memory_resume_percent: self.workflow_resource_throttle_memory_resume_percent,
            disk_high_percent: self.workflow_resource_throttle_disk_high_percent,
            disk_resume_percent: self.workflow_resource_throttle_disk_resume_percent,
        }
    }
}
