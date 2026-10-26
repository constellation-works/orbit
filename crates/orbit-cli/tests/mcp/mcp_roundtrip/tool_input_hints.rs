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

#[test]
fn mcp_unknown_task_tools_suggest_visible_registered_tools_without_aliasing() {
    let workspace = McpWorkspace::init();
    let mut client = workspace.serve();
    for (name, hint) in [
        ("orbit.task.get", Some("orbit.task.show")),
        (
            "orbit.task.artifact.list",
            Some("orbit.task.show with field: \"artifacts\""),
        ),
        ("orbit_task_get", Some("orbit.task.show")),
        (
            "orbit_task_artifact_list",
            Some("orbit.task.show with field: \"artifacts\""),
        ),
        ("orbit.workflow.run.get", None),
        ("orbit.auto_task.get", None),
        ("orbit.definitely.missing", None),
        ("orbit_task_delete", None),
    ] {
        let error = client.call_tool_err(name, json!({}));
        assert_eq!(error["code"], "tool_not_found", "{error}");
        let expected = match hint {
            Some(hint) => format!("tool not found: {name} (did you mean {hint}?)"),
            None => format!("tool not found: {name}"),
        };
        assert_eq!(error["message"], expected);
    }
}
