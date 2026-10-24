//! Last-known-good workflow settings: admission snapshots at pass boundaries,
//! and steering refreshed at prompt and promotion checkpoints.

use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use orbit_config::{AutoConcurrencySettings, MidRunSteeringSettings, ProviderLimitPolicy};

use super::RuntimeKernel;
use crate::config_path::{CONFIG_TOML_FILE, validated_existing_config_root};

#[derive(Clone)]
pub(super) struct AdmissionPolicy {
    auto_concurrency: AutoConcurrencySettings,
    provider_limit: ProviderLimitPolicy,
    mid_run_steering: MidRunSteeringSettings,
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
            Ok(config) => Arc::new(AdmissionPolicy {
                provider_limit: config.snapshot.provider_limit_policy(),
                mid_run_steering: config.snapshot.mid_run_steering(),
                auto_concurrency: config.auto_concurrency,
            }),
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
