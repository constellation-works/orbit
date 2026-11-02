//! [ORB-21263] Role surfaces: each `orbit mcp serve --role` stays within its
//! recorded size budget, so a role's context cost cannot quietly grow, and
//! the always-on core stays limited to workspace discovery.

use orbit_mcp::{canonical_mcp_tool_definitions, mcp_surface_report};
use orbit_types::tool::{McpRole, McpToolset, McpToolsetSelection};
use serde_json::Value;

const BUDGETS: &str = include_str!("mcp_role_budgets.json");

#[test]
fn role_surface_budget() {
    let budgets: Value = serde_json::from_str(BUDGETS).expect("role budgets JSON");
    let surfaces = McpRole::ALL
        .into_iter()
        .map(|role| (role.as_str(), McpToolsetSelection::for_role(role)))
        .chain([("default", McpToolsetSelection::default_only())]);
    let mut over = Vec::new();
    for (name, selection) in surfaces {
        let report = mcp_surface_report(&selection).expect("surface report");
        let budget = &budgets[name];
        let max_tools = budget["max_tools"].as_u64().expect("max_tools") as usize;
        let max_tokens = budget["max_approx_tokens"]
            .as_u64()
            .expect("max_approx_tokens") as usize;
        if report.tools > max_tools || report.approx_tokens > max_tokens {
            over.push(format!(
                "{name}: {} tools / ~{} tokens, budget {max_tools} / {max_tokens}",
                report.tools, report.approx_tokens
            ));
        }
    }
    assert!(
        over.is_empty(),
        "MCP role surfaces over their budget in tests/boundary/mcp_role_budgets.json: {over:?}"
    );
}

#[test]
fn always_on_core_is_workspace_discovery_only() {
    let core = canonical_mcp_tool_definitions()
        .expect("canonical definitions")
        .into_iter()
        .filter(|definition| definition.rules.toolset == McpToolset::Core)
        .map(|definition| definition.schema.name)
        .collect::<Vec<_>>();
    assert_eq!(core, ["orbit.workspace.list"]);
}
