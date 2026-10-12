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
    /// Read operation supporting an adapter-owned presentation.
    pub presentation_read: Option<McpPresentationRead>,
    /// Additional input constraints and workspace taxonomy decoration.
    pub input_decoration: McpInputDecoration,
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
    /// Evaluate the declared conditions without interpreting tool names.
    pub fn applies(&self, input: &Value) -> bool {
        match self {
            Self::Never => false,
            Self::Always => true,
            Self::WhenAny(conditions) => conditions.iter().any(|condition| match condition {
                McpInputCondition::Present(key) => input.get(key).is_some(),
                McpInputCondition::True(key) => input.get(key) == Some(&Value::Bool(true)),
            }),
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
