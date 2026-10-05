//! The same audit regression inputs over the production MCP stdio transport.

use super::*;

#[path = "../../support/tool_input_hints.rs"]
mod tool_input_hint_cases;

#[test]
fn mcp_input_misses_return_actionable_suggestions() {
    let workspace = McpWorkspace::init();
    let mut client = workspace.serve();
    // GitHub builtins have no MCP scope; the CLI matrix exercises both run tools.
    for case in tool_input_hint_cases::cases()
        .into_iter()
        .filter(|case| case.tool.starts_with("orbit."))
    {
        let name = orbit_types::tool::mcp_advertised_tool_name(case.tool);
        let error = client.call_tool_err(&name, case.input.clone());
        tool_input_hint_cases::assert_hint(&case, &error, "message");
        if case.tool == "orbit.search" {
            for suggestion in case.suggestions {
                let mut corrected = case.input.clone();
                corrected["status"] = suggestion.into();
                client.call_tool_ok(&name, corrected);
            }
        }
    }
}
