//! Ship's own `mode` must not trip the auto-drain workspace echo.

use super::*;

/// A bound `orbit.workflow` ship call that passes `mode` and no workspace
/// reaches the same gate as the call that omits `mode`. An `auto` call that
/// carries `mode` still requires an explicit selector.
#[test]
fn ship_mode_without_a_workspace_selector_matches_the_omitted_mode_gate() {
    let workspace = McpWorkspace::init();
    let mut client = workspace.serve_with_args(&["--toolset", "dispatch"]);
    let task_ids = json!(["ORB-00001"]);

    let omitted = client.call_tool_err(
        "orbit_workflow",
        json!({"action": "ship", "task_ids": task_ids, "model": "codex"}),
    );
    assert_eq!(omitted["code"], "capability_denied", "{omitted}");
    assert!(
        !message_contains(&omitted, "requires an explicit workspace selector"),
        "{omitted}"
    );

    for mode in ["local", "pr"] {
        let with_mode = client.call_tool_err(
            "orbit_workflow",
            json!({"action": "ship", "task_ids": task_ids, "mode": mode, "model": "codex"}),
        );
        assert_eq!(
            with_mode["code"], omitted["code"],
            "ship mode {mode} must reach the same gate as the call that omits mode: {with_mode}"
        );
        assert!(
            !message_contains(&with_mode, "requires an explicit workspace selector"),
            "ship mode {mode} must not be refused for a missing workspace selector: {with_mode}"
        );
    }

    let auto = client.call_tool_err(
        "orbit_workflow",
        json!({"action": "auto", "mode": "status", "model": "codex"}),
    );
    assert_eq!(auto["code"], "invalid_input", "{auto}");
    assert!(
        message_contains(&auto, "requires an explicit workspace selector"),
        "{auto}"
    );
}
