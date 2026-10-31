//! The `orbit mcp serve` toolsets: which group of tools a definition is
//! advertised in, and which groups one server enables.

use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::error::McpToolsetError;

/// The toolset an MCP definition belongs to. Every server advertises
/// [`McpToolset::Default`]; the others are opt-in per server. A toolset
/// changes advertisement only: the tools stay registered and `orbit tool
/// run`, activities and plugin callbacks reach them regardless.
#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "kebab-case")]
pub enum McpToolset {
    /// Always advertised.
    #[default]
    Default,
    /// Resource leases.
    Leases,
    /// Experiment definitions, lifecycle and readouts.
    Experiments,
    /// Agent and orchestrator scorecards and the crew ladder.
    Analytics,
    /// A decision channel's own callbacks (submit and digest).
    DecisionChannel,
    /// Agent invocation (`orbit.agent.invoke`). A managed implement or
    /// review-repair run that is offered explorer delegation enables it.
    AgentInvoke,
}

impl McpToolset {
    /// Every toolset, in declaration order.
    pub const ALL: [Self; 6] = [
        Self::Default,
        Self::Leases,
        Self::Experiments,
        Self::Analytics,
        Self::DecisionChannel,
        Self::AgentInvoke,
    ];

    /// The name a selection uses.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Leases => "leases",
            Self::Experiments => "experiments",
            Self::Analytics => "analytics",
            Self::DecisionChannel => "decision-channel",
            Self::AgentInvoke => "agent-invoke",
        }
    }

    fn known_names() -> String {
        Self::ALL
            .iter()
            .map(|toolset| toolset.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl Display for McpToolset {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for McpToolset {
    type Err = McpToolsetError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|toolset| toolset.as_str() == value)
            .ok_or_else(|| McpToolsetError::Unknown {
                name: value.to_string(),
                known: Self::known_names(),
            })
    }
}

/// The toolsets one MCP server advertises. [`McpToolset::Default`] is
/// always enabled, so naming it adds nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpToolsetSelection {
    enabled: BTreeSet<McpToolset>,
}

impl Default for McpToolsetSelection {
    fn default() -> Self {
        Self::default_only()
    }
}

impl McpToolsetSelection {
    /// Only the default toolset.
    pub fn default_only() -> Self {
        Self {
            enabled: BTreeSet::from([McpToolset::Default]),
        }
    }

    /// Every toolset.
    pub fn all() -> Self {
        Self {
            enabled: McpToolset::ALL.into_iter().collect(),
        }
    }

    /// Parse toolset names; `all` enables every toolset.
    pub fn from_names<'a>(
        names: impl IntoIterator<Item = &'a str>,
    ) -> Result<Self, McpToolsetError> {
        let mut selection = Self::default_only();
        for name in names {
            match name.trim() {
                "" => return Err(McpToolsetError::EmptyEntry),
                "all" => selection.enabled.extend(McpToolset::ALL),
                name => {
                    selection.enabled.insert(name.parse()?);
                }
            }
        }
        Ok(selection)
    }

    /// Parse one comma-separated selection, as `--toolset` takes it.
    pub fn parse_list(raw: &str) -> Result<Self, McpToolsetError> {
        Self::from_names(raw.split(','))
    }

    /// This selection with every toolset `other` enables added.
    pub fn union(mut self, other: &Self) -> Self {
        self.enabled.extend(other.enabled.iter().copied());
        self
    }

    /// Whether a definition in `toolset` is advertised.
    pub fn enables(&self, toolset: McpToolset) -> bool {
        self.enabled.contains(&toolset)
    }

    /// The selection as `--toolset` takes it, so a remote server can be
    /// asked for the same one.
    pub fn to_list(&self) -> String {
        let opt_in = self
            .enabled
            .iter()
            .filter(|toolset| **toolset != McpToolset::Default)
            .map(|toolset| toolset.as_str())
            .collect::<Vec<_>>();
        if opt_in.is_empty() {
            McpToolset::Default.as_str().to_string()
        } else {
            opt_in.join(",")
        }
    }
}
