use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::observability::log_rotation::LogRotationConfig;
use orbit_types::identity::Crew;
use orbit_types::workflow::CrewPurpose;
use orbit_types::workflow::automation::members::PreparationFreshness;

use super::machine::{
    AutoConcurrencySettings, MachineSettings, ResourceThrottleSettings, WorkerContainmentSettings,
};
use super::resolve::{DEFAULT_FINAL_RECOVERY_CREWS, resolve_default_crew};
use super::table::ConfigSnapshot;

/// Admitted `workflow.mid_run_steering.*`: whether this workspace records
/// steering watermarks, and the promotion gate's round budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MidRunSteeringSettings {
    /// `workflow.mid_run_steering.enabled` (default `false`).
    pub enabled: bool,
    /// `workflow.mid_run_steering.max_rounds` (default 2).
    pub max_rounds: u8,
}

/// Admitted `workflow.regression_risk.*`: whether implement and review
/// prompts mark regression-prone context files, and the gates a file must
/// pass to be marked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegressionRiskSettings {
    /// `workflow.regression_risk.enabled` (default `true`).
    pub enabled: bool,
    /// `workflow.regression_risk.window_days` (default 30).
    pub window_days: u32,
    /// `workflow.regression_risk.min_support` (default 5).
    pub min_support: u32,
    /// `workflow.regression_risk.threshold_percent` (default 40).
    pub threshold_percent: u8,
    /// `workflow.regression_risk.max_files` (default 3).
    pub max_files: u32,
}

/// Admitted `workflow.explore.*`: whether implement and review-repair runs
/// may delegate read-only exploration through `orbit.agent.invoke`, and the
/// budgets each run's delegation is held to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExploreSettings {
    /// `workflow.explore.enabled` (default `false`).
    pub enabled: bool,
    /// `workflow.explore.max_calls_per_run` (default 6).
    pub max_calls_per_run: u32,
    /// `workflow.explore.max_concurrent_per_run` (default 2).
    pub max_concurrent_per_run: u32,
    /// `workflow.explore.timeout_seconds` (default 600).
    pub timeout_seconds: u32,
    /// `workflow.explore.max_answer_bytes` (default 16384).
    pub max_answer_bytes: u32,
}

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
        for (pool, setting) in [
            (
                &mut self.workflow_purpose_crews_auto_fix,
                "workflow.purpose_crews.auto_fix",
            ),
            (
                &mut self.workflow_purpose_crews_baseline_red_repair,
                "workflow.purpose_crews.baseline_red_repair",
            ),
            (
                &mut self.workflow_purpose_crews_explorer,
                "workflow.purpose_crews.explorer",
            ),
            (
                &mut self.workflow_purpose_crews_failure_recovery,
                "workflow.purpose_crews.failure_recovery",
            ),
            (
                &mut self.workflow_purpose_crews_sweep_tasks,
                "workflow.purpose_crews.sweep_tasks",
            ),
            (
                &mut self.workflow_purpose_crews_task_pilot,
                "workflow.purpose_crews.task_pilot",
            ),
        ] {
            admit_crew_pool(pool, crews, setting)?;
        }
        if let Some(pool) = self.workflow_purpose_crews_review.as_mut() {
            admit_crew_pool(pool, crews, crate::operation::REVIEW_CREW_KEY)?;
        }
        if let Some(pool) = self.workflow_purpose_crews_pre_landing_review.as_mut() {
            admit_crew_pool(pool, crews, crate::operation::PRE_LANDING_REVIEW_CREW_KEY)?;
        }
        self.workflow_purpose_crews_final_recovery = Some(admit_final_recovery_crews(
            self.workflow_purpose_crews_final_recovery.take(),
            crews,
        )?);
        self.workflow_default_crew = resolve_default_crew(
            self.workflow_default_crew.take(),
            crews,
            env_default,
            require_default_crew,
        )?;
        let resources = self.resource_throttle();
        let auto_concurrency = self.auto_concurrency();
        if auto_concurrency.profile_window < auto_concurrency.min_profile_deliveries {
            return Err(OrbitError::InvalidInput(
                "machine.auto_concurrency.profile_window must be at least machine.auto_concurrency.min_profile_deliveries"
                    .to_string(),
            ));
        }
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
            (
                "io_full",
                resources.io_full_high_percent,
                resources.io_full_resume_percent,
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

    /// The admitted `workflow.purpose_crews.final_recovery` pool; empty
    /// disables final recovery. Admission always fills the field, so `None`
    /// never reaches here.
    pub fn final_recovery_crews(&self) -> &[String] {
        self.workflow_purpose_crews_final_recovery
            .as_deref()
            .unwrap_or_default()
    }

    /// The admitted `review.timing`.
    pub fn review_timing(&self) -> crate::operation::ReviewTimingSetting {
        crate::operation::ReviewTimingSetting::parse(&self.review_timing)
            .unwrap_or(crate::operation::ReviewTimingSetting::Off)
    }

    /// The admitted `workflow.validation_env.shell_probe`.
    pub fn shell_probe(&self) -> crate::aliases::ShellProbeMode {
        crate::aliases::ShellProbeMode::parse(&self.workflow_validation_env_shell_probe)
            .unwrap_or(crate::aliases::ShellProbeMode::Interactive)
    }

    /// The admitted `workflow.purpose_crews.<purpose>` pools.
    pub fn purpose_crews(&self) -> crate::PurposeCrewPools {
        crate::PurposeCrewPools::new([
            (
                CrewPurpose::TaskPilot,
                self.workflow_purpose_crews_task_pilot.clone(),
            ),
            (
                CrewPurpose::FailureRecovery,
                self.workflow_purpose_crews_failure_recovery.clone(),
            ),
            (
                CrewPurpose::AutoFix,
                self.workflow_purpose_crews_auto_fix.clone(),
            ),
            (
                CrewPurpose::BaselineRedRepair,
                self.workflow_purpose_crews_baseline_red_repair.clone(),
            ),
            (
                CrewPurpose::SweepTasks,
                self.workflow_purpose_crews_sweep_tasks.clone(),
            ),
            (
                CrewPurpose::Explorer,
                self.workflow_purpose_crews_explorer.clone(),
            ),
        ])
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
        let mode = crate::aliases::WorkerContainmentMode::parse(&self.machine_worker_containment)
            .unwrap_or(crate::aliases::WorkerContainmentMode::Warn);
        WorkerContainmentSettings {
            enabled: mode != crate::aliases::WorkerContainmentMode::Off,
            strict: mode == crate::aliases::WorkerContainmentMode::Strict,
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

/// Admit one `workflow.*_complexity_crews` or `workflow.purpose_crews.*` value
/// in place, replacing it with its canonical `name[:weight]` rendering.
fn admit_crew_pool(
    pool: &mut Vec<String>,
    crews: &BTreeMap<String, Crew>,
    setting: &str,
) -> Result<(), OrbitError> {
    *pool = crate::canonical_crew_pool(pool, crews, setting)?.to_setting_value();
    Ok(())
}

/// Admit `workflow.purpose_crews.final_recovery`.
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
        crate::canonical_crew_pool(&pool, crews, "workflow.purpose_crews.final_recovery")?
            .to_setting_value(),
    )
}

impl ConfigSnapshot {
    /// The admitted host-wide `[machine.auto_concurrency]` thresholds.
    pub fn auto_concurrency(&self) -> AutoConcurrencySettings {
        AutoConcurrencySettings {
            mode: self.machine_auto_concurrency_mode.clone(),
            admit_cpu_percent: self.machine_auto_concurrency_admit_cpu_percent,
            admit_memory_percent: self.machine_auto_concurrency_admit_memory_percent,
            profile_window: self.machine_auto_concurrency_profile_window,
            min_profile_deliveries: self.machine_auto_concurrency_min_profile_deliveries,
            horizon_cap_minutes: self.machine_auto_concurrency_horizon_cap_minutes,
            head_max_wait_minutes: self.machine_auto_concurrency_head_max_wait_minutes,
            head_stuck_passes: self.machine_auto_concurrency_head_stuck_passes,
        }
    }

    /// Admitted `machine.host_shutdown_hold_minutes`.
    pub fn host_shutdown_hold_minutes(&self) -> u32 {
        self.machine_host_shutdown_hold_minutes
    }

    /// The admitted `workflow.mid_run_steering.*` settings.
    pub fn mid_run_steering(&self) -> MidRunSteeringSettings {
        MidRunSteeringSettings {
            enabled: self.workflow_mid_run_steering_enabled,
            max_rounds: self.workflow_mid_run_steering_max_rounds,
        }
    }

    /// The admitted `workflow.explore.*` settings.
    pub fn explore(&self) -> ExploreSettings {
        ExploreSettings {
            enabled: self.workflow_explore_enabled,
            max_calls_per_run: self.workflow_explore_max_calls_per_run,
            max_concurrent_per_run: self.workflow_explore_max_concurrent_per_run,
            timeout_seconds: self.workflow_explore_timeout_seconds,
            max_answer_bytes: self.workflow_explore_max_answer_bytes,
        }
    }

    /// The admitted `workflow.regression_risk.*` settings.
    pub fn regression_risk(&self) -> RegressionRiskSettings {
        RegressionRiskSettings {
            enabled: self.workflow_regression_risk_enabled,
            window_days: self.workflow_regression_risk_window_days,
            min_support: self.workflow_regression_risk_min_support,
            threshold_percent: self.workflow_regression_risk_threshold_percent,
            max_files: self.workflow_regression_risk_max_files,
        }
    }

    /// Admitted host resource thresholds, shared by dashboard and admission consumers.
    pub fn resource_throttle(&self) -> ResourceThrottleSettings {
        ResourceThrottleSettings {
            enabled: self.workflow_resource_throttle_enabled,
            cpu_source: self.workflow_resource_throttle_cpu_source.clone(),
            cpu_high_percent: self.workflow_resource_throttle_cpu_high_percent,
            cpu_resume_percent: self.workflow_resource_throttle_cpu_resume_percent,
            cpu_light_leaves: self.workflow_resource_throttle_cpu_light_leaves,
            memory_high_percent: self.workflow_resource_throttle_memory_high_percent,
            memory_resume_percent: self.workflow_resource_throttle_memory_resume_percent,
            disk_high_percent: self.workflow_resource_throttle_disk_high_percent,
            disk_resume_percent: self.workflow_resource_throttle_disk_resume_percent,
            io_full_high_percent: self.workflow_resource_throttle_io_full_high_percent,
            io_full_resume_percent: self.workflow_resource_throttle_io_full_resume_percent,
        }
    }
}
