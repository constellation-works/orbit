//! When a provider's usage reading keeps delivery admission off its crews
//! (`workflow.provider_limit_*`).
//!
//! The readings themselves live in the host's provider-limit store; these
//! settings only say how close to a window's limit a provider may run before
//! admission skips its crews, and what an explicitly crewed task does then.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_types::identity::Crew;
use orbit_types::workflow::Provider;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{ConfigSnapshot, ProviderLimitBudget};

/// What one `workflow.provider_limit_budgets` or
/// `workflow.provider_limit_overrides` entry covers [ORB-17288].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProviderLimitTarget {
    /// A provider's default login, by canonical name: every crew of that
    /// provider that sets no `env`.
    Provider(String),
    /// One crew that sets `env`, which runs on its own account.
    Crew(String),
}

impl ProviderLimitTarget {
    /// The target as written in an entry: the canonical provider, or the
    /// crew's name.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Provider(name) | Self::Crew(name) => name,
        }
    }

    /// A known provider (an alias resolves to its canonical name), else a
    /// crew of that name. Whether the crew is defined and sets `env` is
    /// checked once the crews are known ([`validate_crew_target`]). A name a
    /// crew could not have keeps the provider's own error.
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        match Provider::parse(raw) {
            Ok(provider) => Ok(Self::Provider(provider.as_str().to_string())),
            Err(_)
                if !raw.is_empty()
                    && !raw.contains(|c: char| c.is_whitespace() || c == SEPARATOR) =>
            {
                Ok(Self::Crew(raw.to_string()))
            }
            Err(error) => Err(error.to_string()),
        }
    }
}

/// [ORB-17288] Refuse a crew target that names no defined crew, or a crew
/// that sets no `env`: only such a crew is an account of its own. The error
/// names the setting, the entry and the crew.
pub(crate) fn validate_crew_target(
    setting: &str,
    entry: &str,
    target: &ProviderLimitTarget,
    crews: &BTreeMap<String, Crew>,
) -> Result<(), OrbitError> {
    let ProviderLimitTarget::Crew(name) = target else {
        return Ok(());
    };
    match crews.get(name) {
        None => {
            let provider = Provider::parse(name)
                .err()
                .map(|error| format!("{error}, and "))
                .unwrap_or_default();
            Err(OrbitError::InvalidInput(format!(
                "{setting} entry `{entry}`: {provider}no crew `{name}` is defined; a crew target \
                 names a `[crews.<name>]` that sets `env`"
            )))
        }
        Some(crew) if crew.account().is_none() => Err(OrbitError::InvalidInput(format!(
            "{setting} entry `{entry}` names crew `{name}`, which sets no `env`: it runs on its \
             provider's default login, which a `{}` entry covers; give \
             `[crews.{name}] env` its own credential, or name the provider",
            crew.assignment.provider
        ))),
        Some(_) => Ok(()),
    }
}

/// `workflow.provider_limit_max_used_pct` when unset.
pub const DEFAULT_PROVIDER_LIMIT_MAX_USED_PCT: u8 = 90;

const OVERRIDES_KEY: &str = "workflow.provider_limit_overrides";
const EXPLICIT_CREWS_KEY: &str = "workflow.provider_limit_explicit_crews";

/// Separates an override's provider from its percent or `off`, as a pool
/// entry's name is separated from its weight.
const SEPARATOR: char = ':';

/// The word that turns one provider's pre-dispatch usage gate off.
const OFF: &str = "off";

/// How close to a window's limit one provider may run, or not at all.
///
/// JSON and the readiness payload write a [`Self::Percent`] as a number and
/// [`Self::Off`] as the string `off`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderLimitThreshold {
    /// Gate at this used percent. `100` gates only an exhausted window.
    Percent(u8),
    /// Record and show readings. Do not gate admission on them.
    Off,
}

impl ProviderLimitThreshold {
    /// The percent, when this gate is one.
    #[must_use]
    pub const fn percent(self) -> Option<u8> {
        match self {
            Self::Percent(percent) => Some(percent),
            Self::Off => None,
        }
    }

    /// Whether admission ignores this provider's readings.
    #[must_use]
    pub const fn is_off(self) -> bool {
        matches!(self, Self::Off)
    }

    fn render(self) -> String {
        match self {
            Self::Percent(percent) => percent.to_string(),
            Self::Off => OFF.to_string(),
        }
    }
}

impl std::fmt::Display for ProviderLimitThreshold {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Percent(percent) => write!(formatter, "{percent}"),
            Self::Off => write!(formatter, "{OFF}"),
        }
    }
}

impl Serialize for ProviderLimitThreshold {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Percent(percent) => serializer.serialize_u8(*percent),
            Self::Off => serializer.serialize_str(OFF),
        }
    }
}

impl<'de> Deserialize<'de> for ProviderLimitThreshold {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl serde::de::Visitor<'_> for Visitor {
            type Value = ProviderLimitThreshold;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a used-percent threshold or \"off\"")
            }

            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                u8::try_from(value)
                    .map(ProviderLimitThreshold::Percent)
                    .map_err(|_| E::custom("threshold percent does not fit in 1..=100"))
            }

            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                u8::try_from(value)
                    .map(ProviderLimitThreshold::Percent)
                    .map_err(|_| E::custom("threshold percent does not fit in 1..=100"))
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                if value == OFF {
                    Ok(ProviderLimitThreshold::Off)
                } else {
                    Err(E::custom(format!("unknown threshold `{value}`")))
                }
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

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
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderLimitPolicy {
    /// The used percent at or above which a provider's crews are skipped.
    pub max_used_pct: u8,
    /// Per-provider gates, by canonical provider name.
    pub overrides: BTreeMap<String, ProviderLimitThreshold>,
    /// [ORB-17288] Per-crew-account gates, by the name of a crew that sets
    /// `env`.
    pub crew_overrides: BTreeMap<String, ProviderLimitThreshold>,
    /// What an explicitly crewed task does when its crew is limited.
    pub explicit_crews: ProviderLimitExplicitCrews,
    /// Operator-declared budgets for providers that report no usage, by
    /// canonical provider [ORB-14699].
    pub budgets: Vec<ProviderLimitBudget>,
    /// How task crew draws weigh weekly usage pressure
    /// (`workflow.crew_selection`, `workflow.usage_pressure_*`).
    pub usage_pressure: UsagePressurePolicy,
}

impl Default for ProviderLimitPolicy {
    fn default() -> Self {
        Self {
            max_used_pct: DEFAULT_PROVIDER_LIMIT_MAX_USED_PCT,
            overrides: BTreeMap::new(),
            crew_overrides: BTreeMap::new(),
            explicit_crews: ProviderLimitExplicitCrews::Wait,
            budgets: Vec::new(),
            usage_pressure: UsagePressurePolicy::default(),
        }
    }
}

/// `workflow.usage_pressure_cap` when unset.
pub const DEFAULT_USAGE_PRESSURE_CAP: f64 = 8.0;
/// `workflow.usage_pressure_strength` when unset.
pub const DEFAULT_USAGE_PRESSURE_STRENGTH: f64 = 1.0;

const CREW_SELECTION_KEY: &str = "workflow.crew_selection";
const USAGE_PRESSURE_CAP_KEY: &str = "workflow.usage_pressure_cap";
const USAGE_PRESSURE_STRENGTH_KEY: &str = "workflow.usage_pressure_strength";

/// How a task's automatic crew draw weighs its pool members
/// (`workflow.crew_selection`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CrewSelection {
    /// Draw on the configured weights alone.
    #[default]
    Static,
    /// Scale each member's weight by its crew's weekly usage pressure.
    UsagePressure,
}

impl CrewSelection {
    /// Every accepted value, as written in `config.toml`.
    pub const CHOICES: [&'static str; 2] = ["static", "usage_pressure"];

    /// The value as written in `config.toml`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::UsagePressure => "usage_pressure",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "static" => Some(Self::Static),
            "usage_pressure" => Some(Self::UsagePressure),
            _ => None,
        }
    }
}

/// The admitted `workflow.crew_selection` and `workflow.usage_pressure_*`
/// settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UsagePressurePolicy {
    /// Whether draws weigh usage pressure at all.
    pub selection: CrewSelection,
    /// The multiplier is clamped to `[1 / cap, cap]`; at least 1.
    pub cap: f64,
    /// The exponent applied to pressure; `0` disables weighting.
    pub strength: f64,
}

impl Default for UsagePressurePolicy {
    fn default() -> Self {
        Self {
            selection: CrewSelection::Static,
            cap: DEFAULT_USAGE_PRESSURE_CAP,
            strength: DEFAULT_USAGE_PRESSURE_STRENGTH,
        }
    }
}

impl UsagePressurePolicy {
    /// Whether draws weigh pressure: `usage_pressure` with a positive
    /// strength.
    #[must_use]
    pub fn applies(&self) -> bool {
        self.selection == CrewSelection::UsagePressure && self.strength > 0.0
    }

    /// The weight multiplier for `pressure`:
    /// `clamp(pressure ^ strength, 1 / cap, cap)`. A non-finite or negative
    /// pressure counts as neutral.
    #[must_use]
    pub fn multiplier(&self, pressure: f64) -> f64 {
        let pressure = if pressure.is_finite() && pressure >= 0.0 {
            pressure
        } else {
            1.0
        };
        pressure.powf(self.strength).clamp(1.0 / self.cap, self.cap)
    }
}

impl ProviderLimitPolicy {
    /// The gate for `provider`, whose label is parsed so an alias such as
    /// `anthropic` finds the `claude` override. A provider with no override
    /// gates at [`Self::max_used_pct`].
    #[must_use]
    pub fn threshold(&self, provider: &str) -> ProviderLimitThreshold {
        let canonical = Provider::parse(provider).map_or(provider, |parsed| parsed.as_str());
        self.overrides
            .get(canonical)
            .copied()
            .unwrap_or(ProviderLimitThreshold::Percent(self.max_used_pct))
    }

    /// [ORB-17288] The gate for one provider account: a crew account's own
    /// override, else its provider's, else [`Self::max_used_pct`]. `None`
    /// is the provider's default login, gated as [`Self::threshold`].
    #[must_use]
    pub fn account_threshold(
        &self,
        provider: &str,
        account: Option<&str>,
    ) -> ProviderLimitThreshold {
        account
            .and_then(|account| self.crew_overrides.get(account).copied())
            .unwrap_or_else(|| self.threshold(provider))
    }

    /// The percent a ledger budget compares spend against.
    ///
    /// `off` turns admission off and is not a percent, so a budget keeps
    /// [`Self::max_used_pct`]. A percent override still retunes the budget.
    #[must_use]
    pub fn budget_threshold(&self, provider: &str, account: Option<&str>) -> u8 {
        self.account_threshold(provider, account)
            .percent()
            .unwrap_or(self.max_used_pct)
    }
}

impl ConfigSnapshot {
    /// The admitted `workflow.provider_limit_*` settings.
    #[must_use]
    pub fn provider_limit_policy(&self) -> ProviderLimitPolicy {
        let mut overrides = BTreeMap::new();
        let mut crew_overrides = BTreeMap::new();
        for (target, threshold) in self
            .workflow_provider_limit_overrides
            .iter()
            .filter_map(|entry| stored_override(entry))
        {
            match target {
                ProviderLimitTarget::Provider(provider) => {
                    overrides.insert(provider, threshold);
                }
                ProviderLimitTarget::Crew(crew) => {
                    crew_overrides.insert(crew, threshold);
                }
            }
        }
        ProviderLimitPolicy {
            max_used_pct: self.workflow_provider_limit_max_used_pct,
            overrides,
            crew_overrides,
            explicit_crews: ProviderLimitExplicitCrews::parse(
                &self.workflow_provider_limit_explicit_crews,
            )
            .unwrap_or_default(),
            budgets: self.provider_limit_budgets(),
            usage_pressure: UsagePressurePolicy {
                selection: CrewSelection::parse(&self.workflow_crew_selection).unwrap_or_default(),
                cap: self.workflow_usage_pressure_cap,
                strength: self.workflow_usage_pressure_strength,
            },
        }
    }
}

/// One admitted override string `target:percent` or `target:off`.
fn stored_override(entry: &str) -> Option<(ProviderLimitTarget, ProviderLimitThreshold)> {
    let (target, value) = entry.split_once(SEPARATOR)?;
    let threshold = if value == OFF {
        ProviderLimitThreshold::Off
    } else {
        ProviderLimitThreshold::Percent(value.parse().ok()?)
    };
    Some((ProviderLimitTarget::parse(target).ok()?, threshold))
}

/// [ORB-17288] Refuse an admitted override whose crew target is not a
/// defined crew that sets `env`.
pub(crate) fn validate_override_crews(
    entries: &[String],
    crews: &BTreeMap<String, Crew>,
) -> Result<(), OrbitError> {
    for entry in entries {
        if let Some((target, _)) = stored_override(entry) {
            validate_crew_target(OVERRIDES_KEY, entry, &target, crews)?;
        }
    }
    Ok(())
}

/// Admit `workflow.provider_limit_overrides`: every entry `target:percent`
/// or `target:off`, the target a known provider (an alias is stored under
/// its canonical name) or [ORB-17288] a crew that sets `env`, named once,
/// and the percent in 1..=100. Stored sorted by target. Crew targets are
/// checked against the crews once they are known.
pub(crate) fn admit_overrides(raw: Option<Vec<String>>) -> Result<Vec<String>, OrbitError> {
    let invalid = |reason: String| OrbitError::InvalidInput(format!("{OVERRIDES_KEY} {reason}"));
    let mut overrides = BTreeMap::new();
    for entry in raw.unwrap_or_default() {
        let Some((target, value)) = entry.trim().split_once(SEPARATOR) else {
            return Err(invalid(format!(
                "entry `{entry}` must be written `provider:percent` or `provider:off`, like every other entry"
            )));
        };
        let target = ProviderLimitTarget::parse(target)
            .map_err(|error| invalid(format!("entry `{entry}`: {error}")))?;
        let value = value.trim();
        let threshold = if value.eq_ignore_ascii_case(OFF) {
            ProviderLimitThreshold::Off
        } else {
            value
                .parse::<u8>()
                .ok()
                .filter(|percent| (1..=100).contains(percent))
                .map(ProviderLimitThreshold::Percent)
                .ok_or_else(|| {
                    invalid(format!(
                        "entry `{entry}` must give a whole percent in 1..=100, or `off`"
                    ))
                })?
        };
        let name = target.name().to_string();
        let kind = match target {
            ProviderLimitTarget::Provider(_) => "provider",
            ProviderLimitTarget::Crew(_) => "crew",
        };
        if overrides.insert(target, threshold).is_some() {
            return Err(invalid(format!("names {kind} `{name}` more than once")));
        }
    }
    Ok(overrides
        .into_iter()
        .map(|(target, threshold)| format!("{}{SEPARATOR}{}", target.name(), threshold.render()))
        .collect())
}

/// Admit `workflow.crew_selection`, `static` when unset.
pub(crate) fn admit_crew_selection(raw: Option<String>) -> Result<String, OrbitError> {
    let value = raw.as_deref().map_or("static", str::trim);
    CrewSelection::parse(value)
        .map(|selection| selection.as_str().to_string())
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "{CREW_SELECTION_KEY} has invalid value '{value}'; expected one of: {}",
                CrewSelection::CHOICES.join(", ")
            ))
        })
}

/// Admit `workflow.usage_pressure_cap`: a finite number of at least 1.
pub(crate) fn admit_usage_pressure_cap(raw: Option<f64>) -> Result<f64, OrbitError> {
    let cap = raw.unwrap_or(DEFAULT_USAGE_PRESSURE_CAP);
    if cap.is_finite() && cap >= 1.0 {
        Ok(cap)
    } else {
        Err(OrbitError::InvalidInput(format!(
            "{USAGE_PRESSURE_CAP_KEY} must be a finite number of at least 1, got {cap}"
        )))
    }
}

/// Admit `workflow.usage_pressure_strength`: a finite number of at least 0.
pub(crate) fn admit_usage_pressure_strength(raw: Option<f64>) -> Result<f64, OrbitError> {
    let strength = raw.unwrap_or(DEFAULT_USAGE_PRESSURE_STRENGTH);
    if strength.is_finite() && strength >= 0.0 {
        Ok(strength)
    } else {
        Err(OrbitError::InvalidInput(format!(
            "{USAGE_PRESSURE_STRENGTH_KEY} must be a finite number of at least 0, got {strength}"
        )))
    }
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
