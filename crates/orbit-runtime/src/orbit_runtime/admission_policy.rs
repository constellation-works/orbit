//! Last-known-good workflow settings: admission snapshots at pass boundaries,
//! and steering refreshed at prompt and promotion checkpoints.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use orbit_config::{
    AutoConcurrencySettings, AutoEffortPolicy, ComplexityCrewPools, MidRunSteeringSettings,
    ProviderLimitPolicy, PurposeCrewPools, ResolvedConfig, StandingDrainSettings,
    TaskTypeDefinition, TypeComplexityCrewPools,
};
use orbit_types::identity::{Crew, HumanCrew};

use super::RuntimeKernel;
use crate::config_path::{CONFIG_TOML_FILE, validated_existing_config_root};

/// Last-known-good crew registry, routing pools and auto-effort policy for
/// admission and crew resolution.
#[derive(Clone)]
pub struct AdmissionCrewSettings {
    pub complexity_crews: ComplexityCrewPools,
    pub crews: BTreeMap<String, Crew>,
    pub human_crews: BTreeMap<String, HumanCrew>,
    pub default_crew: Option<String>,
    pub auto_effort: AutoEffortPolicy,
    pub purpose_crews: PurposeCrewPools,
    pub type_complexity_crews: TypeComplexityCrewPools,
    pub task_types: Vec<TaskTypeDefinition>,
    pub final_recovery_crews: Vec<String>,
}

#[derive(Clone)]
pub(super) struct AdmissionPolicy {
    auto_concurrency: AutoConcurrencySettings,
    provider_limit: ProviderLimitPolicy,
    mid_run_steering: MidRunSteeringSettings,
    /// `experiments.enabled`.
    experiments_enabled: bool,
    /// `workflow.standing_drain.*`.
    standing_drain: StandingDrainSettings,
    crew_settings: Arc<AdmissionCrewSettings>,
}

impl AdmissionPolicy {
    fn from_resolved(config: ResolvedConfig) -> Self {
        let provider_limit = config.snapshot.provider_limit_policy();
        let mid_run_steering = config.snapshot.mid_run_steering();
        let experiments_enabled = config.snapshot.experiments().enabled;
        let standing_drain = config.snapshot.standing_drain();
        let auto_effort = config.snapshot.auto_effort();
        let purpose_crews = config.snapshot.purpose_crews();
        let type_complexity_crews = config.snapshot.type_complexity_crews();
        let task_types = config.snapshot.task_types().to_vec();
        let final_recovery_crews = config.snapshot.final_recovery_crews().to_vec();
        Self {
            provider_limit,
            mid_run_steering,
            experiments_enabled,
            standing_drain,
            auto_concurrency: config.auto_concurrency,
            crew_settings: Arc::new(AdmissionCrewSettings {
                complexity_crews: config.complexity_crews,
                crews: config.crews,
                human_crews: config.human_crews,
                default_crew: config.default_crew,
                auto_effort,
                purpose_crews,
                type_complexity_crews,
                task_types,
                final_recovery_crews,
            }),
        }
    }
}

pub(super) struct CachedPolicy {
    stamps: [Option<(SystemTime, u64)>; 2],
    policy: Arc<AdmissionPolicy>,
}

/// Modification stamp of a layer's `config.toml`. The layer root is validated
/// before the fixed leaf is probed; `None` means the layer has no config file.
fn stamp(root: &Path) -> std::io::Result<Option<(SystemTime, u64)>> {
    let Some(validated_root) = validated_existing_config_root(root)
        .map_err(|error| std::io::Error::other(error.to_string()))?
    else {
        return Ok(None);
    };
    match std::fs::metadata(validated_root.join(CONFIG_TOML_FILE)) {
        Ok(metadata) => Ok(Some((metadata.modified()?, metadata.len()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

impl RuntimeKernel {
    fn current_admission_policy(&self) -> Arc<AdmissionPolicy> {
        if let Some(policy) = &self.admission_policy_snapshot {
            return policy.clone();
        }
        self.refresh_workflow_policy()
    }

    fn refresh_workflow_policy(&self) -> Arc<AdmissionPolicy> {
        let initial = || {
            let settings = self.context.settings();
            Arc::new(AdmissionPolicy {
                auto_concurrency: settings.auto_concurrency().clone(),
                provider_limit: settings.provider_limit().clone(),
                mid_run_steering: settings.mid_run_steering(),
                experiments_enabled: settings.experiments().enabled,
                // Fail closed: the keeper stays off until a config load says
                // otherwise.
                standing_drain: StandingDrainSettings::default(),
                crew_settings: Arc::new(AdmissionCrewSettings {
                    complexity_crews: settings.complexity_crews().clone(),
                    crews: settings.crews().clone(),
                    human_crews: settings.human_crews().clone(),
                    default_crew: settings.default_crew().map(ToOwned::to_owned),
                    auto_effort: settings.auto_effort().clone(),
                    purpose_crews: settings.purpose_crews().clone(),
                    type_complexity_crews: settings.type_complexity_crews().clone(),
                    task_types: settings.task_types().to_vec(),
                    final_recovery_crews: settings.final_recovery_crews().to_vec(),
                }),
            })
        };
        let mut cache = self
            .admission_policy
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let previous = cache
            .as_ref()
            .map_or_else(initial, |cached| cached.policy.clone());
        let stamps = match stamp(&self.global_root())
            .and_then(|global| stamp(&self.shared_root()).map(|workspace| [global, workspace]))
        {
            Ok(stamps) => stamps,
            Err(error) => {
                tracing::warn!(%error, "could not inspect admission configuration; keeping previous policy");
                return previous;
            }
        };
        if cache.as_ref().is_some_and(|cached| cached.stamps == stamps) {
            return previous;
        }
        let policy = match (self.config_loader)() {
            Ok(config) => Arc::new(AdmissionPolicy::from_resolved(config)),
            Err(error) => {
                tracing::warn!(%error, "could not reload admission configuration; keeping previous policy");
                previous
            }
        };
        // Remember failed versions too: warn once, retry when either file changes.
        *cache = Some(CachedPolicy {
            stamps,
            policy: policy.clone(),
        });
        policy
    }

    pub(super) fn current_mid_run_steering(&self) -> MidRunSteeringSettings {
        // Unlike admission decisions, enablement applies to runs already in
        // flight. Do not pin it to a drain pass's admission snapshot.
        self.refresh_workflow_policy().mid_run_steering
    }

    /// Whether experiments may enroll new tasks, from the current config rather
    /// than this runtime's open-time snapshot. Not pinned to a drain pass: an
    /// operator's disable stops new assignments at the next admission, and a
    /// long-lived runtime picks up a later enable.
    pub fn current_experiments_enabled(&self) -> bool {
        self.refresh_workflow_policy().experiments_enabled
    }

    /// `workflow.standing_drain.*` from the current config, like
    /// [`Self::current_experiments_enabled`]: an operator's disable stops the
    /// keeper at its next tick. Off until a config load succeeds.
    pub fn current_standing_drain(&self) -> StandingDrainSettings {
        self.refresh_workflow_policy().standing_drain
    }

    /// Pin both policies for this pass so all decisions use one config version.
    pub fn admission_policy_snapshot(&self) -> Self {
        let mut runtime = self.clone();
        runtime.admission_policy_snapshot = Some(self.current_admission_policy());
        runtime
    }

    pub fn provider_limit_policy(&self) -> ProviderLimitPolicy {
        self.current_admission_policy().provider_limit.clone()
    }

    /// Current last-known-good host settings, or this admission pass's snapshot.
    pub fn auto_concurrency_settings(&self) -> AutoConcurrencySettings {
        self.current_admission_policy().auto_concurrency.clone()
    }

    /// Current last-known-good crew registry, pools and auto-effort policy, or
    /// this admission pass's snapshot.
    pub fn admission_crew_settings(&self) -> Arc<AdmissionCrewSettings> {
        self.current_admission_policy().crew_settings.clone()
    }
}
