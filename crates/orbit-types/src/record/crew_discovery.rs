//! Sanitized crew discovery types shared by MCP hosts and clients.

use serde::{Deserialize, Serialize};

use crate::identity::Crew;
use crate::record::RecordError;
use crate::workflow::activity_job::Provider;

/// Schema version of the [`CrewDiscoveryV1`] projection returned by
/// `orbit.crew.list`.
///
/// Bumped to 2 in ORB-10801, when the entry lost its `backend` field along
/// with the agent execution backend selector it projected, and to 3 when each
/// entry gained `enabled` (`[crews.<name>] enabled`). Disabled crews are
/// listed rather than hidden so a client can offer to enable them.
pub const CREW_DISCOVERY_SCHEMA_VERSION: u32 = 3;

/// One effective crew entry exposed by [`CrewDiscoveryV1`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrewDiscoveryEntryV1 {
    pub name: String,
    pub provider: String,
    pub model: String,
    pub description: Option<String>,
    pub tags: Vec<String>,
    /// Whether dispatch may run this crew. A disabled crew is refused by
    /// selection and dispatch until its table sets `enabled = true`.
    pub enabled: bool,
}

impl CrewDiscoveryEntryV1 {
    pub fn from_crew(crew: &Crew) -> Result<Self, RecordError> {
        let name = required_trimmed("crew name", &crew.name)?;
        let provider = Provider::parse(&crew.assignment.provider).map_err(|error| {
            RecordError::Invalid(format!("crew '{name}' has invalid provider: {error}"))
        })?;
        let model = required_trimmed("crew model", &crew.assignment.model)?;
        // Every crew dispatches through the CLI agent path [ORB-10801], so a
        // provider without a CLI runtime cannot be executed by any crew.
        if !provider.has_cli_runtime() {
            return Err(RecordError::Invalid(format!(
                "crew '{name}' selects provider '{}' without a CLI runtime",
                provider.as_str()
            )));
        }

        let description = crew
            .description
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let mut tags = crew
            .tags
            .iter()
            .map(|tag| tag.trim())
            .filter(|tag| !tag.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        tags.sort();
        tags.dedup();

        Ok(Self {
            name,
            provider: provider.as_str().to_string(),
            model,
            description,
            tags,
            enabled: crew.enabled,
        })
    }
}

/// Sanitized `orbit.crew.list` projection for one selected workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrewDiscoveryV1 {
    pub schema_version: u32,
    pub workspace_id: String,
    pub owner_machine_id: Option<String>,
    pub default_crew: Option<String>,
    pub crews: Vec<CrewDiscoveryEntryV1>,
}

fn required_trimmed(field: &str, value: &str) -> Result<String, RecordError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(RecordError::Invalid(format!("{field} must not be empty")));
    }
    Ok(trimmed.to_string())
}
