//! Last-known-good admission policy, refreshed once at each pass boundary.

use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use orbit_config::{AutoConcurrencySettings, ConfigRoots, ProviderLimitPolicy, ResolvedConfig};

use super::OrbitRuntime;

#[derive(Clone)]
pub(super) struct AdmissionPolicy {
    auto_concurrency: AutoConcurrencySettings,
    provider_limit: ProviderLimitPolicy,
}

pub(super) struct CachedPolicy {
    stamps: [Option<(SystemTime, u64)>; 2],
    policy: Arc<AdmissionPolicy>,
}

fn stamp(path: &Path) -> std::io::Result<Option<(SystemTime, u64)>> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(Some((metadata.modified()?, metadata.len()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

impl OrbitRuntime {
    fn current_admission_policy(&self) -> Arc<AdmissionPolicy> {
        if let Some(policy) = &self.admission_policy_snapshot {
            return policy.clone();
        }
        let initial = || {
            Arc::new(AdmissionPolicy {
                auto_concurrency: self.context.settings().auto_concurrency().clone(),
                provider_limit: self.context.settings().provider_limit().clone(),
            })
        };
        let mut cache = self
            .admission_policy
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let previous = cache
            .as_ref()
            .map_or_else(initial, |cached| cached.policy.clone());
        let roots = ConfigRoots::new(self.global_root(), self.shared_root());
        let stamps = match stamp(&roots.global().join("config.toml")).and_then(|global| {
            stamp(&roots.workspace().join("config.toml")).map(|workspace| [global, workspace])
        }) {
            Ok(stamps) => stamps,
            Err(error) => {
                tracing::warn!(%error, "could not inspect admission configuration; keeping previous policy");
                return previous;
            }
        };
        if cache.as_ref().is_some_and(|cached| cached.stamps == stamps) {
            return previous;
        }
        let policy = match ResolvedConfig::load(&roots) {
            Ok(config) => Arc::new(AdmissionPolicy {
                provider_limit: config.snapshot.provider_limit_policy(),
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

    /// Pin both policies for this pass so all decisions use one config version.
    pub(crate) fn admission_policy_snapshot(&self) -> Self {
        let mut runtime = self.clone();
        runtime.admission_policy_snapshot = Some(self.current_admission_policy());
        runtime
    }

    pub(crate) fn provider_limit_policy(&self) -> ProviderLimitPolicy {
        self.current_admission_policy().provider_limit.clone()
    }

    /// Current last-known-good host settings, or this admission pass's snapshot.
    pub fn auto_concurrency_settings(&self) -> AutoConcurrencySettings {
        self.current_admission_policy().auto_concurrency.clone()
    }
}
