use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::observability::log_rotation::LogRotationConfig;
use orbit_types::identity::Crew;
use orbit_types::workflow::automation::members::PreparationFreshness;
use orbit_types::workflow::{
    ConventionTemplate, CrewPurpose, DeliveryConventions, MissingSectionPolicy,
    PrBodyTemplateSource, ScreenshotSection, TemplateSurface,
};

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

/// Admitted `experiments.*`: the workspace's experiment policy. Experiment
/// definitions are store records, not configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct ExperimentSettings {
    /// `experiments.enabled` (default `false`).
    pub enabled: bool,
    /// `experiments.max_running` (default 3).
    pub max_running: u32,
    /// `experiments.config_keys` (default empty).
    pub config_keys: Vec<String>,
    /// `experiments.min_delivered_rate` (default 0.9): below this share of
    /// a treatment arm's settled tasks receiving its treatment, a readout
    /// withholds its verdict.
    pub min_delivered_rate: f64,
}

/// Admitted `notice.*`: fleet-notice expiry defaults and caps, and the size,
/// open-count and rate limits a post is held to. Who may post or close, and
/// the kinds, are code, not configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoticeSettings {
    /// `notice.default_expiry_hours.{incident,maintenance,info}` (72, 12, 168).
    pub default_expiry_hours: NoticeKindHours,
    /// `notice.max_expiry_hours.operator` (default 720).
    pub max_expiry_hours_operator: u32,
    /// `notice.max_expiry_hours.orchestrator.*` (168, 72, 168).
    pub max_expiry_hours_orchestrator: NoticeKindHours,
    /// `notice.max_expiry_hours.agent` (default 24): also the agent default.
    pub max_expiry_hours_agent: u32,
    /// `notice.max_title_chars` (default 120).
    pub max_title_chars: u32,
    /// `notice.max_body_bytes` (default 4096).
    pub max_body_bytes: u32,
    /// `notice.max_evidence` (default 8).
    pub max_evidence: u32,
    /// `notice.max_open_per_workspace` (default 20).
    pub max_open_per_workspace: u32,
    /// `notice.max_open_global_per_owner` (default 10).
    pub max_open_global_per_owner: u32,
    /// `notice.max_open_per_agent_task` (default 2).
    pub max_open_per_agent_task: u32,
    /// `notice.agent_posts_per_task_per_day` (default 5).
    pub agent_posts_per_task_per_day: u32,
    /// `notice.orchestrator_posts_per_hour` (default 30).
    pub orchestrator_posts_per_hour: u32,
    /// `notice.retention_days` (default 30).
    pub retention_days: u32,
    /// `notice.agent_posting` (default `true`): managed agents may post
    /// and close their own notices through their bound task.
    pub agent_posting: bool,
    /// `notice.brief_delivery` (default `true`): quote open notices into
    /// opted-in activities' prompts as untrusted data.
    pub brief_delivery: bool,
    /// `notice.brief_max_items` (default 5).
    pub brief_max_items: u32,
    /// `notice.brief_body_chars` (default 600).
    pub brief_body_chars: u32,
    /// `notice.brief_read_timeout_ms` (default 2000).
    pub brief_read_timeout_ms: u32,
}

/// One hour value per notice kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoticeKindHours {
    /// Hours for an `incident` notice.
    pub incident: u32,
    /// Hours for a `maintenance` notice.
    pub maintenance: u32,
    /// Hours for an `info` notice.
    pub info: u32,
}

impl Default for NoticeSettings {
    fn default() -> Self {
        Self {
            default_expiry_hours: NoticeKindHours {
                incident: 72,
                maintenance: 12,
                info: 168,
            },
            max_expiry_hours_operator: 720,
            max_expiry_hours_orchestrator: NoticeKindHours {
                incident: 168,
                maintenance: 72,
                info: 168,
            },
            max_expiry_hours_agent: 24,
            max_title_chars: 120,
            max_body_bytes: 4096,
            max_evidence: 8,
            max_open_per_workspace: 20,
            max_open_global_per_owner: 10,
            max_open_per_agent_task: 2,
            agent_posts_per_task_per_day: 5,
            orchestrator_posts_per_hour: 30,
            retention_days: 30,
            agent_posting: true,
            brief_delivery: true,
            brief_max_items: 5,
            brief_body_chars: 600,
            brief_read_timeout_ms: 2000,
        }
    }
}

/// Which document an admission sees: a layered load (both files merged, or
/// one effective layer), or one file read alone, whose cross-layer
/// references the layered load checks instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdmissionScope {
    /// The merged or effective document.
    Layered,
    /// One file alone: `workflow.type_complexity_crews` may name a type the
    /// other layer's `workflow.task_types` defines.
    File,
}

impl ConfigSnapshot {
    pub(super) fn finish_admission(
        &mut self,
        crews: &BTreeMap<String, Crew>,
        env_default: Option<&str>,
        require_default_crew: bool,
        scope: AdmissionScope,
    ) -> Result<(), OrbitError> {
        LogRotationConfig::from_parts(
            Some(self.runtime_log_retention_days),
            Some(self.runtime_log_max_total_mb),
            Some(self.runtime_log_max_file_mb),
        )?;
        crate::crew_auto_effort::admit_auto_effort_bands(&self.workflow_auto_effort_bands, crews)?;
        // `<crew>@auto` needs bands for its crew. A single-file view may
        // take them from the other layer, so it checks only where the entry
        // may appear; the layered load checks the bands.
        let auto_bands = self.workflow_auto_effort_bands.clone();
        let auto = AutoEntries::Admitted((scope == AdmissionScope::Layered).then_some(&auto_bands));
        for (pool, setting) in [
            (
                &mut self.workflow_low_complexity_crews,
                "workflow.low_complexity_crews",
            ),
            (
                &mut self.workflow_medium_complexity_crews,
                "workflow.medium_complexity_crews",
            ),
            (
                &mut self.workflow_hard_complexity_crews,
                "workflow.hard_complexity_crews",
            ),
            (
                &mut self.workflow_xhard_complexity_crews,
                "workflow.xhard_complexity_crews",
            ),
            (
                &mut self.workflow_purpose_crews_task_pilot,
                "workflow.purpose_crews.task_pilot",
            ),
        ] {
            admit_crew_pool(pool, crews, setting, auto)?;
        }
        crate::crew_pools::admit_type_complexity_crews(
            &mut self.workflow_type_complexity_crews,
            (scope == AdmissionScope::Layered).then_some(self.workflow_task_types.as_slice()),
            crews,
            match auto {
                AutoEntries::Admitted(bands) => bands,
                AutoEntries::Refused => None,
            },
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
                &mut self.workflow_purpose_crews_failure_recovery,
                "workflow.purpose_crews.failure_recovery",
            ),
            (
                &mut self.workflow_purpose_crews_pilot_warning_triage,
                "workflow.purpose_crews.pilot_warning_triage",
            ),
            (
                &mut self.workflow_purpose_crews_sweep_tasks,
                "workflow.purpose_crews.sweep_tasks",
            ),
            (
                &mut self.workflow_purpose_crews_visual_review,
                "workflow.purpose_crews.visual_review",
            ),
        ] {
            admit_crew_pool(pool, crews, setting, AutoEntries::Refused)?;
        }
        if let Some(pool) = self.workflow_purpose_crews_review.as_mut() {
            admit_crew_pool(pool, crews, crate::operation::REVIEW_CREW_KEY, auto)?;
        }
        if let Some(pool) = self.workflow_purpose_crews_pre_landing_review.as_mut() {
            admit_crew_pool(
                pool,
                crews,
                crate::operation::PRE_LANDING_REVIEW_CREW_KEY,
                auto,
            )?;
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
        crate::provider_limit::validate_budget_crews(&self.workflow_provider_limit_budgets, crews)?;
        crate::provider_limit::validate_override_crews(
            &self.workflow_provider_limit_overrides,
            crews,
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

    /// The admitted selective before-landing policy, or `None` while its
    /// mode is `off`. Provenance and timing-conflict validation live in
    /// the layered [`crate::OperationPolicy`] an admission captures; this
    /// snapshot view carries the same predicates without layer labels.
    pub fn selective_before_landing_policy(
        &self,
    ) -> Option<orbit_types::workflow::SelectiveBeforeLandingPolicy> {
        let mode =
            crate::operation::selective_mode_parse(&self.review_selective_before_landing_mode)
                .unwrap_or(orbit_types::workflow::SelectiveBeforeLandingMode::Off);
        if !mode.is_active() {
            return None;
        }
        let unknown_risk = crate::operation::selective_unknown_risk_parse(
            &self.review_selective_before_landing_unknown_risk,
        )
        .unwrap_or_default();
        Some(orbit_types::workflow::SelectiveBeforeLandingPolicy::new(
            mode,
            self.review_selective_before_landing_risk_threshold.clone(),
            self.review_selective_before_landing_paths.clone(),
            unknown_risk,
            std::collections::BTreeMap::new(),
        ))
    }

    /// The admitted `workflow.validation_env.shell_probe`.
    pub fn shell_probe(&self) -> crate::aliases::ShellProbeMode {
        crate::aliases::ShellProbeMode::parse(&self.workflow_validation_env_shell_probe)
            .unwrap_or(crate::aliases::ShellProbeMode::Interactive)
    }

    /// The admitted `workflow.admission_affinity`.
    pub fn admission_affinity(&self) -> orbit_types::task::AdmissionAffinityReach {
        orbit_types::task::AdmissionAffinityReach::parse(&self.workflow_admission_affinity)
            .unwrap_or_default()
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
                CrewPurpose::VisualReview,
                self.workflow_purpose_crews_visual_review.clone(),
            ),
            (
                CrewPurpose::PilotWarningTriage,
                self.workflow_purpose_crews_pilot_warning_triage.clone(),
            ),
        ])
    }

    /// The admitted `workflow.auto_effort.*` policy that resolves
    /// `<crew>@auto` pool entries.
    pub fn auto_effort(&self) -> crate::AutoEffortPolicy {
        crate::AutoEffortPolicy::new(
            self.workflow_auto_effort_bands.clone(),
            self.workflow_auto_effort_explore_share,
        )
    }

    /// The admitted `workflow.type_complexity_crews.<type>.<tier>` pools.
    pub fn type_complexity_crews(&self) -> crate::TypeComplexityCrewPools {
        crate::TypeComplexityCrewPools::from_table(&self.workflow_type_complexity_crews)
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
    crate::crew_defaults::builtin_default_crews()
}

/// Whether a pool's key admits `<crew>@auto` entries, and the bands they
/// need (`None`: a single-file view, whose bands the layered load checks).
#[derive(Clone, Copy)]
enum AutoEntries<'a> {
    Admitted(Option<&'a BTreeMap<String, Vec<crate::AutoEffortBand>>>),
    Refused,
}

/// Admit one `workflow.*_complexity_crews` or `workflow.purpose_crews.*` value
/// in place, replacing it with its canonical `name[:weight]` rendering.
fn admit_crew_pool(
    pool: &mut Vec<String>,
    crews: &BTreeMap<String, Crew>,
    setting: &str,
    auto: AutoEntries<'_>,
) -> Result<(), OrbitError> {
    *pool = crate::canonical_crew_pool(pool, crews, setting)?.to_setting_value();
    match auto {
        AutoEntries::Refused => {
            crate::crew_auto_effort::check_auto_effort_entries(pool, setting, None)
        }
        AutoEntries::Admitted(Some(bands)) => {
            crate::crew_auto_effort::check_auto_effort_entries(pool, setting, Some(bands))
        }
        AutoEntries::Admitted(None) => Ok(()),
    }
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
    let pool = crate::canonical_crew_pool(&pool, crews, "workflow.purpose_crews.final_recovery")?
        .to_setting_value();
    crate::crew_auto_effort::check_auto_effort_entries(
        &pool,
        "workflow.purpose_crews.final_recovery",
        None,
    )?;
    Ok(pool)
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

    /// The admitted `workflow.lock_modes` entries, compiled. Admission has
    /// already compiled every pattern, so an error here is a snapshot built
    /// around admission.
    pub fn footprint_lock_modes(
        &self,
    ) -> Result<orbit_common::fs::lock_mode::FootprintLockModes, OrbitError> {
        orbit_common::fs::lock_mode::FootprintLockModes::new(&self.workflow_lock_modes).map_err(
            |reason| OrbitError::InvalidInput(format!("{} {reason}", crate::LOCK_MODES_KEY)),
        )
    }

    /// The admitted `workflow.mid_run_steering.*` settings.
    pub fn mid_run_steering(&self) -> MidRunSteeringSettings {
        MidRunSteeringSettings {
            enabled: self.workflow_mid_run_steering_enabled,
            max_rounds: self.workflow_mid_run_steering_max_rounds,
        }
    }

    /// The admitted `experiments.*` settings.
    pub fn experiments(&self) -> ExperimentSettings {
        ExperimentSettings {
            enabled: self.experiments_enabled,
            max_running: self.experiments_max_running,
            config_keys: self.experiments_config_keys.clone(),
            min_delivered_rate: self.experiments_min_delivered_rate,
        }
    }

    /// The admitted `notice.*` settings.
    pub fn notice(&self) -> NoticeSettings {
        NoticeSettings {
            default_expiry_hours: NoticeKindHours {
                incident: self.notice_default_expiry_hours_incident,
                maintenance: self.notice_default_expiry_hours_maintenance,
                info: self.notice_default_expiry_hours_info,
            },
            max_expiry_hours_operator: self.notice_max_expiry_hours_operator,
            max_expiry_hours_orchestrator: NoticeKindHours {
                incident: self.notice_max_expiry_hours_orchestrator_incident,
                maintenance: self.notice_max_expiry_hours_orchestrator_maintenance,
                info: self.notice_max_expiry_hours_orchestrator_info,
            },
            max_expiry_hours_agent: self.notice_max_expiry_hours_agent,
            max_title_chars: self.notice_max_title_chars,
            max_body_bytes: self.notice_max_body_bytes,
            max_evidence: self.notice_max_evidence,
            max_open_per_workspace: self.notice_max_open_per_workspace,
            max_open_global_per_owner: self.notice_max_open_global_per_owner,
            max_open_per_agent_task: self.notice_max_open_per_agent_task,
            agent_posts_per_task_per_day: self.notice_agent_posts_per_task_per_day,
            orchestrator_posts_per_hour: self.notice_orchestrator_posts_per_hour,
            retention_days: self.notice_retention_days,
            agent_posting: self.notice_agent_posting,
            brief_delivery: self.notice_brief_delivery,
            brief_max_items: self.notice_brief_max_items,
            brief_body_chars: self.notice_brief_body_chars,
            brief_read_timeout_ms: self.notice_brief_read_timeout_ms,
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

impl ConfigSnapshot {
    /// The admitted `pr.*`, `commit.*` and `branch.*` delivery conventions.
    /// Admission already parsed every template, so each parses again here.
    pub fn delivery_conventions(&self) -> DeliveryConventions {
        let text = |key: &str, raw: &Option<String>| {
            raw.as_deref()
                .and_then(|raw| ConventionTemplate::parse(key, raw, TemplateSurface::Text).ok())
        };
        let defaults = DeliveryConventions::default();
        DeliveryConventions {
            pr_title: text("pr.title_template", &self.pr_title_template),
            pr_body: self
                .pr_body_template
                .as_deref()
                .and_then(|raw| PrBodyTemplateSource::parse("pr.body_template", raw).ok()),
            agent_sections: self.pr_agent_sections.clone(),
            required_sections: self.pr_required_sections.clone(),
            missing_section: MissingSectionPolicy::parse(&self.pr_missing_required_section)
                .unwrap_or_default(),
            screenshot_section: ScreenshotSection::parse(&self.pr_screenshot_section)
                .unwrap_or_default(),
            freshness_section: self.pr_freshness_section,
            signature: self.pr_signature,
            pr_orchestration_trailer: self.pr_orchestration_trailer,
            labels: self.pr_labels.clone(),
            task_tag_labels: self.pr_task_tag_labels.clone(),
            task_type_labels: self.pr_task_type_labels.clone(),
            reviewers: self.pr_reviewers.clone(),
            draft: self.pr_draft,
            commit_subject: self.commit_subject_template.as_deref().and_then(|raw| {
                ConventionTemplate::parse_commit_subject("commit.subject_template", raw).ok()
            }),
            commit_body: text("commit.body_template", &self.commit_body_template),
            orchestrated_by_trailer: self.commit_orchestrated_by_trailer,
            regression_trailers: self.commit_regression_trailers,
            branch_name: self
                .branch_name_template
                .as_deref()
                .and_then(|raw| ConventionTemplate::parse_branch("branch.name_template", raw).ok())
                .unwrap_or(defaults.branch_name),
        }
    }
}
