//! Provider and crew-account targets shared by thresholds and budgets.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_types::identity::Crew;
use orbit_types::workflow::Provider;

/// Separates an override's provider from its percent or `off`, as a pool
/// entry's name is separated from its weight.
pub(super) const SEPARATOR: char = ':';

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
