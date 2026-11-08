//! Last-known-good workflow settings: admission snapshots at pass boundaries,
//! and steering refreshed at prompt and promotion checkpoints.

use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use orbit_config::{
    AutoConcurrencySettings, MidRunSteeringSettings, ProviderLimitPolicy, ResolvedConfig,
};

use super::RuntimeKernel;
use crate::config_path::{CONFIG_TOML_FILE, validated_existing_config_root};

#[derive(Clone)]
pub(super) struct AdmissionPolicy {
    auto_concurrency: AutoConcurrencySettings,
    provider_limit: ProviderLimitPolicy,
    mid_run_steering: MidRunSteeringSettings,
    /// `experiments.enabled`.
    experiments_enabled: bool,
    /// `crew_memory.enabled`.
    crew_memory_enabled: bool,
}

impl AdmissionPolicy {
    fn from_resolved(config: ResolvedConfig) -> Self {
        Self {
            provider_limit: config.snapshot.provider_limit_policy(),
            mid_run_steering: config.snapshot.mid_run_steering(),
            experiments_enabled: config.snapshot.experiments().enabled,
            crew_memory_enabled: config.snapshot.crew_memory_enabled(),
            auto_concurrency: config.auto_concurrency,
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
            Arc::new(AdmissionPolicy {
                auto_concurrency: self.context.settings().auto_concurrency().clone(),
                provider_limit: self.context.settings().provider_limit().clone(),
                mid_run_steering: self.context.settings().mid_run_steering(),
                experiments_enabled: self.context.settings().experiments().enabled,
                // Fail closed: until a config load succeeds, crew memory
                // writes stay refused, the feature's default.
                crew_memory_enabled: false,
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

    /// Whether crew memory writes are admitted, from the current config like
    /// [`Self::current_experiments_enabled`], so an operator's disable stops
    /// every write at once, including in a long-lived runtime.
    pub fn current_crew_memory_enabled(&self) -> bool {
        self.refresh_workflow_policy().crew_memory_enabled
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
