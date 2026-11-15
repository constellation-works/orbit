//! The mandate artifact and the rights a resident's authority may hold.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{RESIDENT_MANDATE_SCHEMA, RESIDENT_MAX_EXIT_CRITERIA, RESIDENT_MIN_FLOOR_MINUTES};
use crate::workflow::ResidentMandateError;

/// Longest one-line mandate field (goal, exit criterion), in characters.
const MAX_LINE_CHARS: usize = 240;

/// One right a resident's authority may hold. Anything else is refused.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ResidentRight {
    /// Start or requeue leaf runs and drains.
    Dispatch,
    /// Move a `proposed` task to `backlog`.
    Promote,
    /// Land a reviewed pull request.
    Merge,
    /// Install or upgrade a workspace plugin.
    PluginUpgrade,
}

impl ResidentRight {
    /// Every right, in contract order.
    pub const ALL: [Self; 4] = [
        Self::Dispatch,
        Self::Promote,
        Self::Merge,
        Self::PluginUpgrade,
    ];
    /// The right names, for refusal messages.
    pub const NAMES: &'static str = "dispatch, promote, merge or plugin_upgrade";

    /// Stable right name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dispatch => "dispatch",
            Self::Promote => "promote",
            Self::Merge => "merge",
            Self::PluginUpgrade => "plugin_upgrade",
        }
    }

    /// The right a stable name denotes, if it denotes one.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|right| right.as_str() == name.trim())
    }
}

/// What the mandate watches beside the tasks the resident dispatched or filed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResidentWatch {
    /// Task ids whose changes feed the wake digest.
    #[serde(default)]
    pub tasks: Vec<String>,
    /// Tags whose tasks' changes feed the wake digest.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// How often a resident may wake.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResidentCadence {
    /// Fewest minutes between two cycles.
    pub floor_minutes: u32,
    /// Most minutes without a cycle.
    pub heartbeat_minutes: u32,
}

/// Whether a resident resumes a provider conversation between cycles.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResidentContinuity {
    /// Resume a provider conversation when the adapter can.
    #[default]
    Prefer,
    /// Start every cycle fresh.
    Off,
}

/// The `resident/mandate.json` artifact, schema version 1.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResidentMandate {
    /// Always [`RESIDENT_MANDATE_SCHEMA`].
    pub schema: String,
    /// One line; the task description keeps the narrative.
    pub goal: String,
    /// One to eight lines.
    pub exit_criteria: Vec<String>,
    /// Past it the resident only hands off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<DateTime<Utc>>,
    /// Workspaces the mandate is about; a superset of the authority's scope.
    pub workspaces: Vec<String>,
    /// Tasks and tags feeding the wake digest.
    #[serde(default)]
    pub watch: ResidentWatch,
    /// A registered crew.
    pub crew: String,
    /// Cadence floor and heartbeat.
    pub cadence: ResidentCadence,
    /// Conversation continuity preference.
    #[serde(default)]
    pub continuity: ResidentContinuity,
    /// Whether the mandate is run interactively as a probe.
    #[serde(default)]
    pub probe: bool,
}

impl ResidentMandate {
    /// Parse and validate artifact bytes.
    pub fn parse(bytes: &[u8]) -> Result<Self, ResidentMandateError> {
        let mandate: Self = serde_json::from_slice(bytes)
            .map_err(|error| ResidentMandateError::Malformed(error.to_string()))?;
        mandate.validate()?;
        Ok(mandate)
    }

    /// Check every field rule the type alone can judge. Crew registration
    /// and the host's clock cadence are judged by core.
    pub fn validate(&self) -> Result<(), ResidentMandateError> {
        if self.schema != RESIDENT_MANDATE_SCHEMA {
            return Err(ResidentMandateError::Schema(self.schema.clone()));
        }
        one_line(&self.goal, "goal")?;
        if !(1..=RESIDENT_MAX_EXIT_CRITERIA).contains(&self.exit_criteria.len()) {
            return Err(ResidentMandateError::ExitCriteriaCount(
                self.exit_criteria.len(),
            ));
        }
        for (index, criterion) in self.exit_criteria.iter().enumerate() {
            one_line(criterion, &format!("exit_criteria[{index}]"))?;
        }
        if self.workspaces.is_empty() {
            return Err(ResidentMandateError::Empty("workspaces"));
        }
        let mut seen = BTreeSet::new();
        for workspace in &self.workspaces {
            if workspace.trim().is_empty() {
                return Err(ResidentMandateError::Empty("workspaces"));
            }
            if !seen.insert(workspace.trim()) {
                return Err(ResidentMandateError::DuplicateWorkspace(
                    workspace.trim().to_string(),
                ));
            }
        }
        if self.crew.trim().is_empty() {
            return Err(ResidentMandateError::Empty("crew"));
        }
        if self.cadence.floor_minutes < RESIDENT_MIN_FLOOR_MINUTES {
            return Err(ResidentMandateError::Cadence(format!(
                "mandate `cadence.floor_minutes` must be at least {RESIDENT_MIN_FLOOR_MINUTES}"
            )));
        }
        if self.cadence.heartbeat_minutes < self.cadence.floor_minutes {
            return Err(ResidentMandateError::Cadence(
                "mandate `cadence.heartbeat_minutes` must be at least `cadence.floor_minutes`"
                    .into(),
            ));
        }
        Ok(())
    }

    /// Whether `workspace` is one the mandate is about.
    pub fn covers_workspace(&self, workspace: &str) -> bool {
        self.workspaces
            .iter()
            .any(|candidate| candidate.trim() == workspace.trim())
    }
}

fn one_line(value: &str, field: &str) -> Result<(), ResidentMandateError> {
    if value.trim().is_empty() {
        return Err(ResidentMandateError::NotOneLine(field.to_string()));
    }
    if value.contains(['\n', '\r']) || value.chars().count() > MAX_LINE_CHARS {
        return Err(ResidentMandateError::NotOneLine(field.to_string()));
    }
    Ok(())
}
