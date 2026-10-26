//! Workflow keys explicitly admitted for task-local experiment overrides.

use orbit_common::OrbitError;
use serde_json::Value;

/// A workflow key whose type and per-run consumer are opted in by code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExperimentConfigKey {
    /// Maximum read-only explorer calls a managed run may submit (1–100).
    ExploreMaxCallsPerRun,
}

impl ExperimentConfigKey {
    /// The complete code allowlist; workspace permission is also required.
    pub const ALL: &[Self] = &[Self::ExploreMaxCallsPerRun];

    /// The dotted configuration key.
    pub const fn config_key(self) -> &'static str {
        match self {
            Self::ExploreMaxCallsPerRun => "workflow.explore.max_calls_per_run",
        }
    }

    /// Resolve an opted-in key, refusing every other workflow setting.
    pub fn from_config_key(key: &str) -> Result<Self, OrbitError> {
        Self::ALL
            .iter()
            .copied()
            .find(|entry| entry.config_key() == key)
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "config key `{key}` is not on the experiment code allowlist"
                ))
            })
    }

    /// Admit the key's JSON type and bounds without changing workspace config.
    pub fn validate_experiment_value(self, value: &Value) -> Result<(), OrbitError> {
        let valid = match self {
            Self::ExploreMaxCallsPerRun => value
                .as_u64()
                .is_some_and(|value| (1..=100).contains(&value)),
        };
        if valid {
            Ok(())
        } else {
            Err(OrbitError::InvalidInput(format!(
                "experiment config `{}` must be an integer from 1 to 100",
                self.config_key()
            )))
        }
    }
}
