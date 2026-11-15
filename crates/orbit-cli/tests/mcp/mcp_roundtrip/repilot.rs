//! `orbit_task_recover` action `repilot` over MCP reports refusals and runtime errors with their
//! own stable codes, never as `invalid_input` [ORB-22908].
use super::*;

#[test]
fn mcp_repilot_keeps_the_refusal_code_and_the_not_found_kind() {
    let workspace = McpWorkspace::init();
    let task_id = author_task(&workspace, "Archived repilot fixture");
    cli_ok(
        &workspace.work,
        &workspace.home,
        &["task", "update", &task_id, "--status", "archived"],
    );
    let mut client = workspace.serve_with_args(&["--operator"]);
    let refused = client.assert_tool_error(
        "orbit_task_recover",
        json!({"action": "repilot", "id": task_id}),
        "task_pilot_unsupported_status",
    );
    assert!(
        refused["message"]
            .as_str()
            .is_some_and(|message| message.contains("only proposed and backlog")),
        "{refused}"
    );
    client.assert_tool_error(
        "orbit_task_recover",
        json!({"action": "repilot", "id": "ORB-99999"}),
        "not_found",
    );
}
