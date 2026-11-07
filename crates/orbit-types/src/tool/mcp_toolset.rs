//! The `orbit mcp serve` toolsets and roles: which group of tools a
//! definition is advertised in, which groups one server enables, and the
//! named roles that bundle groups for one kind of session.

use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::error::McpToolsetError;

/// The toolset an MCP definition belongs to. Every tool belongs to exactly
/// one. [`McpToolset::Core`] is always advertised; the
/// [`McpToolset::DEFAULT_TIER`] toolsets are advertised when no role is
/// configured; the others are opt-in per server. A toolset changes
/// advertisement only: the tools stay registered and `orbit tool run`,
/// activities and plugin callbacks reach them regardless.
#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "kebab-case")]
pub enum McpToolset {
    /// Always advertised: workspace discovery.
    #[default]
    Core,
    /// Task, artifact and friction reads.
    TasksRead,
    /// Task, artifact and friction writes.
    TasksWrite,
    /// Delivery repair verbs (requeue, review reset, handoff recovery, …).
    TaskRepair,
    /// Decision requests: list, show and raise.
    Decisions,
    /// Workflow run reads.
    Runs,
    /// Workflow dispatch: ship, auto, resume and pipeline invocation.
    Dispatch,
    /// Auto-tasks, routines and the condition-trigger explain.
    Automation,
    /// Codebase analytics reads.
    Codebase,
    /// Lexical search over tasks and frictions.
    Search,
    /// Visual capture.
    Ui,
    /// Command execution.
    Exec,
    /// Plugin and external tools.
    Plugins,
    /// Resource leases.
    Leases,
    /// Experiment definitions, lifecycle and readouts.
    Experiments,
    /// Agent and orchestrator scorecards and the crew ladder.
    Analytics,
    /// A decision channel's own callbacks (submit and digest).
    DecisionChannel,
    /// Fleet notices: post, close, list and show.
    Notice,
    /// Resident mandates: the authority grant, relay and control, show,
    /// list, checkpoint, metrics and digest, and the evolution ledger.
    Resident,
    /// Tools no MCP server advertises: `orbit tool run` and managed
    /// activities reach them. Naming it in a selection advertises nothing.
    Local,
}

impl McpToolset {
    /// The toolset ORB-20383 removed with `orbit.agent.invoke`. Selections
    /// ignore it with a warning so stored `machine.mcp.toolsets` entries
    /// keep resolving; no toolset replaces it.
    pub const RETIRED_AGENT_INVOKE: &str = "agent-invoke";

    /// Every retired toolset name a selection ignores.
    pub const RETIRED_TOOLSETS: [&'static str; 1] = [Self::RETIRED_AGENT_INVOKE];

    /// Migration note for the retired toolset, shared by every surface that
    /// still accepts it.
    pub const RETIRED_AGENT_INVOKE_NOTE: &str = "agent invocation (orbit.agent.invoke) was removed; remove 'agent-invoke' from the selection";

    /// Whether `name` is a retired toolset selections ignore (trimmed, exact).
    pub fn is_retired_toolset_name(name: &str) -> bool {
        Self::RETIRED_TOOLSETS.contains(&name.trim())
    }

    /// Every toolset, in declaration order.
    pub const ALL: [Self; 20] = [
        Self::Core,
        Self::TasksRead,
        Self::TasksWrite,
        Self::TaskRepair,
        Self::Decisions,
        Self::Runs,
        Self::Dispatch,
        Self::Automation,
        Self::Codebase,
        Self::Search,
        Self::Ui,
        Self::Exec,
        Self::Plugins,
        Self::Leases,
        Self::Experiments,
        Self::Analytics,
        Self::DecisionChannel,
        Self::Notice,
        Self::Resident,
        Self::Local,
    ];

    /// The toolsets a server with no role advertises beside
    /// [`McpToolset::Core`]: together, the surface that was once the single
    /// `default` toolset. The selection name `default` stands for them.
    pub const DEFAULT_TIER: [Self; 12] = [
        Self::TasksRead,
        Self::TasksWrite,
        Self::TaskRepair,
        Self::Decisions,
        Self::Runs,
        Self::Dispatch,
        Self::Automation,
        Self::Codebase,
        Self::Search,
        Self::Ui,
        Self::Exec,
        Self::Plugins,
    ];

    /// The name a selection uses.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::TasksRead => "tasks-read",
            Self::TasksWrite => "tasks-write",
            Self::TaskRepair => "task-repair",
            Self::Decisions => "decisions",
            Self::Runs => "runs",
            Self::Dispatch => "dispatch",
            Self::Automation => "automation",
            Self::Codebase => "codebase",
            Self::Search => "search",
            Self::Ui => "ui",
            Self::Exec => "exec",
            Self::Plugins => "plugins",
            Self::Leases => "leases",
            Self::Experiments => "experiments",
            Self::Analytics => "analytics",
            Self::DecisionChannel => "decision-channel",
            Self::Notice => "notice",
            Self::Resident => "resident",
            Self::Local => "local",
        }
    }

    /// Whether a server with no role advertises this toolset.
    pub fn in_default_tier(self) -> bool {
        self == Self::Core || Self::DEFAULT_TIER.contains(&self)
    }

    /// The roles whose surface includes this toolset, in declaration order.
    pub fn roles(self) -> Vec<McpRole> {
        McpRole::ALL
            .into_iter()
            .filter(|role| role.includes(self))
            .collect()
    }

    fn known_names() -> String {
        Self::ALL
            .iter()
            .map(|toolset| toolset.as_str())
            .chain(["default"])
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
        if Self::is_retired_toolset_name(value) {
            return Err(McpToolsetError::Retired {
                name: value.trim().to_string(),
                note: Self::RETIRED_AGENT_INVOKE_NOTE,
            });
        }
        Self::ALL
            .into_iter()
            .find(|toolset| toolset.as_str() == value)
            .ok_or_else(|| McpToolsetError::Unknown {
                name: value.to_string(),
                known: Self::known_names(),
            })
    }
}

/// A named set of toolsets for one kind of MCP session. A role limits what a
/// server advertises and dispatches; it never grants authority, so an
/// operator-only tool still needs operator capability under every role.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum McpRole {
    /// Every toolset: the full surface.
    Operator,
    /// A supervising manager: read-only task and run views, decisions
    /// (attention), notices, resident mandates and search.
    Manager,
    /// The resident orchestrator's bounded cycle: task reads and writes, run
    /// reads, dispatch, decisions, notices, its mandate and search.
    Resident,
    /// A managed agent, aligned with the managed activity baseline allowlist.
    Agent,
    /// Read-only views.
    Reader,
}

impl McpRole {
    /// Every role, in declaration order.
    pub const ALL: [Self; 5] = [
        Self::Operator,
        Self::Manager,
        Self::Resident,
        Self::Agent,
        Self::Reader,
    ];

    /// The name `--role` and `machine.mcp.role` take.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Operator => "operator",
            Self::Manager => "manager",
            Self::Resident => "resident",
            Self::Agent => "agent",
            Self::Reader => "reader",
        }
    }

    /// The toolsets this role serves beside [`McpToolset::Core`]. The one
    /// place a role's surface is defined.
    pub fn toolsets(self) -> &'static [McpToolset] {
        use McpToolset as T;
        match self {
            Self::Operator => &McpToolset::ALL,
            Self::Manager => &[
                T::TasksRead,
                T::Runs,
                T::Decisions,
                T::Notice,
                T::Resident,
                T::Search,
            ],
            Self::Resident => &[
                T::TasksRead,
                T::TasksWrite,
                T::Runs,
                T::Dispatch,
                T::Decisions,
                T::Notice,
                T::Resident,
                T::Search,
            ],
            Self::Agent => &[
                T::TasksRead,
                T::TasksWrite,
                T::Decisions,
                T::Codebase,
                T::Search,
                T::Ui,
                T::Leases,
                T::Notice,
            ],
            Self::Reader => &[T::TasksRead, T::Runs, T::Codebase, T::Search],
        }
    }

    /// Whether this role's surface includes `toolset`.
    pub fn includes(self, toolset: McpToolset) -> bool {
        toolset == McpToolset::Core || self.toolsets().contains(&toolset)
    }

    fn known_names() -> String {
        Self::ALL
            .iter()
            .map(|role| role.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl Display for McpRole {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for McpRole {
    type Err = McpToolsetError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        Self::ALL
            .into_iter()
            .find(|role| role.as_str() == value)
            .ok_or_else(|| McpToolsetError::UnknownRole {
                name: value.to_string(),
                known: Self::known_names(),
            })
    }
}

/// The toolsets one MCP server advertises, and the role they came from.
/// [`McpToolset::Core`] is always enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpToolsetSelection {
    enabled: BTreeSet<McpToolset>,
    role: Option<McpRole>,
}

impl Default for McpToolsetSelection {
    fn default() -> Self {
        Self::default_only()
    }
}

impl McpToolsetSelection {
    /// The surface with no role: core and the default tier.
    pub fn default_only() -> Self {
        Self {
            enabled: McpToolset::ALL
                .into_iter()
                .filter(|toolset| toolset.in_default_tier())
                .collect(),
            role: None,
        }
    }

    /// Every toolset.
    pub fn all() -> Self {
        Self {
            enabled: McpToolset::ALL.into_iter().collect(),
            role: None,
        }
    }

    /// Exactly the toolsets `role` serves.
    pub fn for_role(role: McpRole) -> Self {
        let mut enabled = BTreeSet::from([McpToolset::Core]);
        enabled.extend(role.toolsets().iter().copied());
        Self {
            enabled,
            role: Some(role),
        }
    }

    /// Parse toolset names onto the no-role surface; `all` enables every
    /// toolset and `default` the default tier.
    pub fn from_names<'a>(
        names: impl IntoIterator<Item = &'a str>,
    ) -> Result<Self, McpToolsetError> {
        Self::default_only().with_names(names)
    }

    /// This selection with the named toolsets added; `all` adds every
    /// toolset and `default` the default tier. The role is kept. A retired
    /// toolset name is ignored so stored selections keep loading; callers
    /// warn naming the removal.
    pub fn with_names<'a>(
        mut self,
        names: impl IntoIterator<Item = &'a str>,
    ) -> Result<Self, McpToolsetError> {
        for name in names {
            match name.trim() {
                "" => return Err(McpToolsetError::EmptyEntry),
                "all" => self.enabled.extend(McpToolset::ALL),
                "default" => self.enabled.extend(McpToolset::DEFAULT_TIER),
                name if McpToolset::is_retired_toolset_name(name) => {}
                name => {
                    self.enabled.insert(name.parse()?);
                }
            }
        }
        Ok(self)
    }

    /// This selection with every toolset `other` enables added. The role is
    /// kept.
    pub fn union(mut self, other: &Self) -> Self {
        self.enabled.extend(other.enabled.iter().copied());
        self
    }

    /// Whether a definition in `toolset` is advertised.
    pub fn enables(&self, toolset: McpToolset) -> bool {
        toolset == McpToolset::Core || self.enabled.contains(&toolset)
    }

    /// Whether every toolset is enabled, so nothing is withheld.
    pub fn enables_all(&self) -> bool {
        McpToolset::ALL
            .into_iter()
            .all(|toolset| self.enables(toolset))
    }

    /// The role this selection serves, when one was configured.
    pub fn role(&self) -> Option<McpRole> {
        self.role
    }

    /// The enabled toolsets, in declaration order.
    pub fn toolsets(&self) -> impl Iterator<Item = McpToolset> + '_ {
        self.enabled.iter().copied()
    }

    /// The selection as `--toolset` takes it beside `--role`, so a remote
    /// server can be asked for the same one: the toolsets beyond the role,
    /// or beyond the default tier without one. With no role and nothing
    /// beyond the default tier this is `default`; with a role and nothing
    /// beyond it, `None`.
    pub fn to_list(&self) -> Option<String> {
        let extra = self
            .enabled
            .iter()
            .filter(|toolset| match self.role {
                Some(role) => !role.includes(**toolset),
                None => !toolset.in_default_tier(),
            })
            .map(|toolset| toolset.as_str())
            .collect::<Vec<_>>();
        match (extra.is_empty(), self.role) {
            (false, _) => Some(extra.join(",")),
            (true, None) => Some("default".to_string()),
            (true, Some(_)) => None,
        }
    }
}
