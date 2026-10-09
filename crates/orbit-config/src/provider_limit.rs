//! When a provider's usage reading keeps delivery admission off its crews
//! (`workflow.provider_limit_*`).
//!
//! The readings themselves live in the host's provider-limit store; these
//! settings only say how close to a window's limit a provider may run before
//! admission skips its crews, and what an explicitly crewed task does then.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_types::workflow::Provider;

use crate::ConfigSnapshot;

/// `workflow.provider_limit_max_used_pct` when unset.
pub const DEFAULT_PROVIDER_LIMIT_MAX_USED_PCT: u8 = 90;

const OVERRIDES_KEY: &str = "workflow.provider_limit_overrides";
const EXPLICIT_CREWS_KEY: &str = "workflow.provider_limit_explicit_crews";

/// Separates an override's provider from its percent, as a pool entry's name
/// is separated from its weight.
const SEPARATOR: char = ':';

/// What admission does with a task whose explicit crew is limited
/// (`workflow.provider_limit_explicit_crews`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProviderLimitExplicitCrews {
    /// The task waits in the backlog until the limit lifts.
    #[default]
    Wait,
    /// The task is drawn from the unlimited members of its complexity pool.
    Pool,
}

impl ProviderLimitExplicitCrews {
    /// Every accepted value, as written in `config.toml`.
    pub const CHOICES: [&'static str; 2] = ["wait", "pool"];

    /// The value as written in `config.toml`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Wait => "wait",
            Self::Pool => "pool",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "wait" => Some(Self::Wait),
            "pool" => Some(Self::Pool),
            _ => None,
        }
    }
}

/// The admitted `workflow.provider_limit_*` settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderLimitPolicy {
    /// The used percent at or above which a provider's crews are skipped.
    pub max_used_pct: u8,
    /// Per-provider thresholds, by canonical provider name.
    pub overrides: BTreeMap<String, u8>,
    /// What an explicitly crewed task does when its crew is limited.
    pub explicit_crews: ProviderLimitExplicitCrews,
}

impl Default for ProviderLimitPolicy {
    fn default() -> Self {
        Self {
            max_used_pct: DEFAULT_PROVIDER_LIMIT_MAX_USED_PCT,
            overrides: BTreeMap::new(),
            explicit_crews: ProviderLimitExplicitCrews::Wait,
        }
    }
}

impl ProviderLimitPolicy {
    /// The threshold for `provider`, whose label is parsed so an alias such
    /// as `anthropic` finds the `claude` override.
    #[must_use]
    pub fn threshold(&self, provider: &str) -> u8 {
        let canonical = Provider::parse(provider).map_or(provider, |parsed| parsed.as_str());
        self.overrides
            .get(canonical)
            .copied()
            .unwrap_or(self.max_used_pct)
    }
}

impl ConfigSnapshot {
    /// The admitted `workflow.provider_limit_*` settings.
    #[must_use]
    pub fn provider_limit_policy(&self) -> ProviderLimitPolicy {
        ProviderLimitPolicy {
            max_used_pct: self.workflow_provider_limit_max_used_pct,
            overrides: self
                .workflow_provider_limit_overrides
                .iter()
                .filter_map(|entry| {
                    let (provider, percent) = entry.split_once(SEPARATOR)?;
                    Some((provider.to_string(), percent.parse().ok()?))
                })
                .collect(),
            explicit_crews: ProviderLimitExplicitCrews::parse(
                &self.workflow_provider_limit_explicit_crews,
            )
            .unwrap_or_default(),
        }
    }
}

/// Admit `workflow.provider_limit_overrides`: every entry `provider:percent`,
/// the provider a known one (an alias is stored under its canonical name),
/// named once, and the percent in 1..=100. Stored sorted by provider.
pub(crate) fn admit_overrides(raw: Option<Vec<String>>) -> Result<Vec<String>, OrbitError> {
    let invalid = |reason: String| OrbitError::InvalidInput(format!("{OVERRIDES_KEY} {reason}"));
    let mut overrides = BTreeMap::new();
    for entry in raw.unwrap_or_default() {
        let Some((provider, percent)) = entry.trim().split_once(SEPARATOR) else {
            return Err(invalid(format!(
                "entry `{entry}` must be written `provider:percent`, like every other entry"
            )));
        };
        let provider = Provider::parse(provider.trim())
            .map_err(|error| invalid(format!("entry `{entry}`: {error}")))?;
        let percent = percent
            .trim()
            .parse::<u8>()
            .ok()
            .filter(|percent| (1..=100).contains(percent))
            .ok_or_else(|| {
                invalid(format!(
                    "entry `{entry}` must give a whole percent in 1..=100"
                ))
            })?;
        if overrides.insert(provider.as_str(), percent).is_some() {
            return Err(invalid(format!(
                "names provider `{}` more than once",
                provider.as_str()
            )));
        }
    }
    Ok(overrides
        .into_iter()
        .map(|(provider, percent)| format!("{provider}{SEPARATOR}{percent}"))
        .collect())
}

/// Admit `workflow.provider_limit_explicit_crews`, `wait` when unset.
pub(crate) fn admit_explicit_crews(raw: Option<String>) -> Result<String, OrbitError> {
    let value = raw.as_deref().map_or("wait", str::trim);
    ProviderLimitExplicitCrews::parse(value)
        .map(|policy| policy.as_str().to_string())
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "{EXPLICIT_CREWS_KEY} has invalid value '{value}'; expected one of: {}",
                ProviderLimitExplicitCrews::CHOICES.join(", ")
            ))
        })
}
