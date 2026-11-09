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

/// The selector refusal for an `auto` call that carries `mode` must not
/// mention `view`, which the caller never set. An explicit selector clears
/// the gate, and `auto` without `mode` keeps using the bound workspace.
#[test]
fn auto_mode_selector_refusal_does_not_advise_omitting_view() {
    let workspace = McpWorkspace::init();
    let mut client = workspace.serve_with_args(&["--toolset", "dispatch"]);

    let refused = client.call_tool_err(
        "orbit_workflow",
        json!({"action": "auto", "mode": "status", "model": "codex"}),
    );
    assert_eq!(refused["code"], "invalid_input", "{refused}");
    assert!(
        message_contains(&refused, "requires an explicit workspace selector"),
        "{refused}"
    );
    assert!(
        !message_contains(&refused, "`view`"),
        "the caller never set `view`, so the refusal must not name it: {refused}"
    );

    let selected = client.call_tool_err(
        "orbit_workflow",
        json!({
            "action": "auto",
            "mode": "status",
            "workspace": "ws_mcp-roundtrip",
            "model": "codex"
        }),
    );
    assert_eq!(selected["code"], "capability_denied", "{selected}");
    assert!(
        !message_contains(&selected, "requires an explicit workspace selector"),
        "an explicit selector must pass the selector gate: {selected}"
    );

    let bound = client.call_tool_err(
        "orbit_workflow",
        json!({"action": "auto", "model": "codex"}),
    );
    assert_eq!(bound["code"], "capability_denied", "{bound}");
    assert!(
        !message_contains(&bound, "requires an explicit workspace selector"),
        "auto without mode must use the bound workspace: {bound}"
    );
}
