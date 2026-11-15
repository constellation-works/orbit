//! Host-declared MCP behavior, separate from client-facing annotations.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Transport rules carried by a definition, never advertised in `tools/list`.
/// Defaults preserve ordinary tool behavior for older or plugin definitions.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpToolRules {
    /// Calls matching this condition require and echo the explicit selector.
    pub workspace_echo: McpWorkspaceEcho,
    /// Routing classification for tools addressing a single id.
    pub id_route: McpIdRoute,
    /// Keep the tool's own authoritative workspace selector schema verbatim.
    pub declared_workspace_selector: bool,
    /// The selector may be omitted: the answering server then reads every
    /// workspace it reaches (a federated session aggregates its owners; a
    /// single-host server answers its owner-local aggregate, which the host
    /// requests by stamping `all: true` on the call).
    #[serde(default)]
    pub workspace_optional: bool,
    /// Read operation supporting an adapter-owned presentation.
    pub presentation_read: Option<McpPresentationRead>,
    /// Additional input constraints and workspace taxonomy decoration.
    pub input_decoration: McpInputDecoration,
    /// The `orbit mcp serve` toolset that advertises this tool. For a tool
    /// family, the toolset of its first action.
    #[serde(default)]
    pub toolset: super::McpToolset,
    /// A tool family's toolset per action, as `(action, toolset)`. A family
    /// is advertised when any action's toolset is enabled, and a call is
    /// served only when its own action's is. Empty for any other tool.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub action_toolsets: Vec<(String, super::McpToolset)>,
}

impl McpToolRules {
    /// The toolset serving `action` of a family, else the tool's toolset.
    pub fn toolset_for_action(&self, action: Option<&str>) -> super::McpToolset {
        action
            .and_then(|action| {
                self.action_toolsets
                    .iter()
                    .find(|(name, _)| name == action)
                    .map(|(_, toolset)| *toolset)
            })
            .unwrap_or(self.toolset)
    }
}

/// When the adapter must require and echo a workspace selector.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum McpWorkspaceEcho {
    /// Ordinary host routing, without an adapter-owned selector echo.
    #[default]
    Never,
    /// Every invocation requires and echoes an explicit selector.
    Always,
    /// Any matching input condition enables the explicit selector contract.
    WhenAny(Vec<McpInputCondition>),
}

impl McpWorkspaceEcho {
    /// Input fields in matching echo conditions, in declaration order.
    pub fn matching_input_fields<'a>(&'a self, input: &Value) -> Vec<&'a str> {
        let mut fields = Vec::new();
        if let Self::WhenAny(conditions) = self {
            for condition in conditions.iter().filter(|condition| condition.holds(input)) {
                condition.collect_fields(&mut fields);
            }
        }
        fields.dedup();
        fields
    }

    /// Evaluate the declared conditions without interpreting tool names.
    pub fn applies(&self, input: &Value) -> bool {
        match self {
            Self::Never => false,
            Self::Always => true,
            Self::WhenAny(conditions) => conditions.iter().any(|condition| condition.holds(input)),
        }
    }
}

/// A condition on a call's arguments; presence includes null and false.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum McpInputCondition {
    /// The argument is present, regardless of its value.
    Present(String),
    /// The argument is exactly the JSON boolean true.
    True(String),
    /// The argument is exactly this JSON string.
    Equals {
        /// Argument name.
        key: String,
        /// Required string value.
        value: String,
    },
    /// Every nested condition matches. An empty list matches nothing, so a
    /// vacant conjunction cannot enable an echo.
    All(Vec<McpInputCondition>),
}

impl McpInputCondition {
    fn collect_fields<'a>(&'a self, fields: &mut Vec<&'a str>) {
        match self {
            Self::Present(key) | Self::True(key) | Self::Equals { key, .. } => {
                if !fields.contains(&key.as_str()) {
                    fields.push(key);
                }
            }
            Self::All(conditions) => {
                for condition in conditions {
                    condition.collect_fields(fields);
                }
            }
        }
    }

    fn holds(&self, input: &Value) -> bool {
        match self {
            Self::Present(key) => input.get(key).is_some(),
            Self::True(key) => input.get(key) == Some(&Value::Bool(true)),
            Self::Equals { key, value } => input.get(key).and_then(Value::as_str) == Some(value),
            Self::All(conditions) => {
                !conditions.is_empty() && conditions.iter().all(|condition| condition.holds(input))
            }
        }
    }
}

/// A tool definition's declaration of its [`McpIdRoute`], in `const` form.
///
/// Operation specs and other builtin definitions are `const`, so they cannot
/// carry the owned reason string of [`McpIdRoute::WorkspaceLocal`]; they
/// declare this form at the definition site and the registry resolves it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum McpIdRouteDeclaration {
    /// The tool does not declare an id routing contract.
    #[default]
    Unspecified,
    /// The id names one globally addressed task.
    Task,
    /// The id is interpreted at the selected workspace, with an explicit reason.
    WorkspaceLocal(&'static str),
}

impl McpIdRouteDeclaration {
    /// Resolve the declaration to the routing classification consumers use.
    pub fn resolve(self) -> McpIdRoute {
        match self {
            Self::Unspecified => McpIdRoute::Unspecified,
            Self::Task => McpIdRoute::Task,
            Self::WorkspaceLocal(reason) => McpIdRoute::WorkspaceLocal(reason.to_string()),
        }
    }
}

/// Whether an id-only invocation routes by a task's host prefix.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum McpIdRoute {
    /// The tool does not declare an id routing contract.
    #[default]
    Unspecified,
    /// The id names one globally addressed task.
    Task,
    /// The id is interpreted at the selected workspace, with an explicit reason.
    WorkspaceLocal(String),
}

/// Data read supplied by a definition to the read-only presentation adapter.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum McpPresentationRead {
    /// A task read accepting the panel's field projection.
    Task,
    /// A run read accepting the bounded view.
    Run,
}

/// Schema decoration requested by the owning tool definition.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum McpInputDecoration {
    /// No extra decoration beyond the tool's declared input.
    #[default]
    None,
    /// Friction title bound and the bound workspace's tag taxonomy.
    Friction,
    /// Nonempty search filters and the query/tag/friction-kind constraint.
    Search,
}
