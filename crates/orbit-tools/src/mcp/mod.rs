//! Canonical MCP annotations, workspace-echo rules, and toolset membership for tools; imports `task_guarded_selectors`, `tool`, and `orbit-types` (never `registry`).

mod annotations;
mod rules;
mod toolsets;

pub(crate) use annotations::mcp_annotations;
pub(crate) use rules::{builtin_mcp_rules_with_route, mcp_rules};
pub(crate) use toolsets::builtin_mcp_toolset;
