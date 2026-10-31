//! Last-known-good workflow settings: admission snapshots at pass boundaries,
//! and steering refreshed at prompt and promotion checkpoints.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use orbit_config::{
    AutoConcurrencySettings, ComplexityCrewPools, CrewLadderSettings, MidRunSteeringSettings,
    ProviderLimitPolicy, ResolvedConfig,
};
use orbit_types::identity::Crew;

use super::RuntimeKernel;
use crate::config_path::{CONFIG_TOML_FILE, validated_existing_config_root};

#[derive(Clone)]
pub(super) struct AdmissionPolicy {
    auto_concurrency: AutoConcurrencySettings,
    provider_limit: ProviderLimitPolicy,
    mid_run_steering: MidRunSteeringSettings,
    crew_ladder: Arc<CrewLadderControls>,
}

/// The owner's current control settings for the crew ladder's trial sync and
/// report: the ladder, the crew registry and complexity pools its plan reads,
/// and the experiment gates. One value is one config version, so a plan never
/// mixes a newer ladder mode with an older pool. Running workers keep the
/// contracts they froze at admission; nothing here reaches them.
#[derive(Debug, Clone, PartialEq)]
pub struct CrewLadderControls {
    pub settings: CrewLadderSettings,
    pub crews: BTreeMap<String, Crew>,
    pub complexity_crews: ComplexityCrewPools,
    /// `experiments.enabled`.
    pub experiments_enabled: bool,
    /// `experiments.max_running`.
    pub max_running: u32,
}

impl AdmissionPolicy {
    fn from_resolved(config: ResolvedConfig) -> Self {
        let experiments = config.snapshot.experiments();
        Self {
            provider_limit: config.snapshot.provider_limit_policy(),
            mid_run_steering: config.snapshot.mid_run_steering(),
            crew_ladder: Arc::new(CrewLadderControls {
                settings: config.snapshot.crew_ladder(),
                crews: config.crews,
                complexity_crews: config.complexity_crews,
                experiments_enabled: experiments.enabled,
                max_running: experiments.max_running,
            }),
            auto_concurrency: config.auto_concurrency,
        }
    }
}

pub(super) struct CachedPolicy {
    stamps: [Option<(SystemTime, u64)>; 2],
    policy: Arc<AdmissionPolicy>,
    reload_valid: bool,
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
        self.refresh_workflow_policy_with_status().0
    }

    fn refresh_workflow_policy_with_status(&self) -> (Arc<AdmissionPolicy>, bool) {
        let initial = || {
            Arc::new(AdmissionPolicy {
                auto_concurrency: self.context.settings().auto_concurrency().clone(),
                provider_limit: self.context.settings().provider_limit().clone(),
                mid_run_steering: self.context.settings().mid_run_steering(),
                crew_ladder: Arc::new(CrewLadderControls {
                    settings: self.context.settings().crew_ladder().clone(),
                    crews: self.context.settings().crews().clone(),
                    complexity_crews: self.context.settings().complexity_crews().clone(),
                    experiments_enabled: self.context.settings().experiments().enabled,
                    max_running: self.context.settings().experiments().max_running,
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
                return (previous, false);
            }
        };
        if let Some(cached) = cache.as_ref().filter(|cached| cached.stamps == stamps) {
            return (previous, cached.reload_valid);
        }
        let (policy, reload_valid) = match (self.config_loader)() {
            Ok(config) => (Arc::new(AdmissionPolicy::from_resolved(config)), true),
            Err(error) => {
                tracing::warn!(%error, "could not reload admission configuration; keeping previous policy");
                (previous, false)
            }
        };
        // Remember failed versions too: warn once, retry when either file changes.
        *cache = Some(CachedPolicy {
            stamps,
            policy: policy.clone(),
            reload_valid,
        });
        (policy, reload_valid)
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
        self.refresh_workflow_policy()
            .crew_ladder
            .experiments_enabled
    }

    /// The current ladder controls, from the current config rather than this
    /// runtime's open-time snapshot, or this admission pass's snapshot. A
    /// long-lived runtime opened before an operator changed the ladder must
    /// not conclude a trial the current config still wants. Invalid config
    /// keeps the last-known-good controls.
    pub fn current_crew_ladder_controls(&self) -> Arc<CrewLadderControls> {
        self.current_admission_policy().crew_ladder.clone()
    }

    /// Freshly validated owner controls, or none when the current layers cannot load.
    pub fn readable_crew_ladder_controls(&self) -> Option<Arc<CrewLadderControls>> {
        let (policy, valid) = self.refresh_workflow_policy_with_status();
        valid.then(|| policy.crew_ladder.clone())
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
}
