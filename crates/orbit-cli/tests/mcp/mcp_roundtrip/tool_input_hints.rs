//! The same audit regression inputs over the production MCP stdio transport.

#[path = "../../support/tool_input_hints.rs"]
mod tool_input_hint_cases;

use super::*;

#[test]
fn mcp_missing_records_name_a_recovery_tool_or_field() {
    let workspace = McpWorkspace::init();
    let mut client = workspace.serve_with_args(&["--toolset", "decisions"]);
    let task_id = client.add_task("Delivery lookup", "Missing-run recovery fixture");
    for (tool, input, code, hint) in [
        (
            "orbit_task_show",
            json!({"id": "TST-99999999"}),
            "not_found",
            "`orbit_task_list {}`",
        ),
        (
            "orbit_task_show",
            json!({"id": task_id, "field": "delivery", "run_id": "missing-run"}),
            "not_found",
            "`orbit_run {\"action\":\"list\"}`",
        ),
        (
            "orbit_decision",
            json!({"action": "show", "id": "missing-request"}),
            "request_not_found",
            "`orbit.decision` with action `list`",
        ),
    ] {
        let error = client.call_tool_err(tool, input);
        assert_eq!(error["code"], code, "{error}");
        assert!(error["message"].as_str().unwrap().contains(hint), "{error}");
    }
}

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
            None => format!(
                "tool not found: {name}; see `orbit tool list` or MCP `tools/list` for valid tool names"
            ),
        };
        assert_eq!(error["message"], expected);
    }
}

#[test]
fn mcp_task_tools_accept_task_id_alias_without_advertising_it() {
    let workspace = McpWorkspace::init();
    let mut client = workspace.serve_with_args(&["--operator", "--role", "agent"]);
    let task_id = client.add_task("Task id alias", "Exercise task_id alias");

    // 1. tools/list never advertises `task_id` on any task tool.
    let listed = client.tools_list();
    for tool in listed["result"]["tools"].as_array().expect("tools array") {
        let name = tool["name"].as_str().unwrap_or_default();
        if name.starts_with("orbit_task_") {
            assert!(
                tool["inputSchema"]["properties"].get("task_id").is_none(),
                "{name} must not advertise `task_id` in tools/list: {tool}"
            );
        }
    }

    // 2. orbit_task_show with `task_id` behaves identically to `id`.
    let by_id = client.call_tool_ok("orbit_task_show", json!({"id": task_id}));
    let by_task_id = client.call_tool_ok("orbit_task_show", json!({"task_id": task_id}));
    assert_eq!(by_task_id, by_id);

    // 3. orbit_task_update with `task_id` updates identically to `id` (both
    //    ordinary and guarded modes).
    let updated_by_task_id = client.call_tool_ok(
        "orbit_task_update",
        json!({"task_id": task_id, "priority": "high"}),
    );
    assert_eq!(updated_by_task_id["id"], task_id);
    assert_eq!(updated_by_task_id["priority"], "high");

    let snapshot = client.call_tool_ok(
        "orbit_task_show",
        json!({"task_id": task_id, "snapshot": true, "workspace": workspace.work}),
    );
    let revision = snapshot["revision"].as_str().expect("snapshot revision");
    let guarded = client.call_tool_ok(
        "orbit_task_update",
        json!({
            "workspace": workspace.work,
            "request_id": "req-alias-guarded-comment",
            "expected_revision": revision,
            "task_id": task_id,
            "comment": "Guarded comment via task_id",
        }),
    );
    assert_eq!(
        guarded["snapshot"]["comments"][0]["message"], "Guarded comment via task_id",
        "{guarded}"
    );

    // 4. orbit_task_artifact (put and get) with `task_id` behaves identically
    //    to `id`.
    let source = workspace.work.join(".orbit/tmp/task-id-alias.txt");
    std::fs::create_dir_all(source.parent().expect("tmp parent")).expect("create tmp dir");
    std::fs::write(&source, "alias artifact bytes\n").expect("write source");
    client.call_tool_ok(
        "orbit_task_artifact",
        json!({
            "action": "put",
            "task_id": task_id,
            "source_path": source,
            "path": "alias.txt",
        }),
    );
    let by_id_art = client.call_tool_ok(
        "orbit_task_artifact",
        json!({"action": "get", "id": task_id, "path": "alias.txt"}),
    );
    let by_task_id_art = client.call_tool_ok(
        "orbit_task_artifact",
        json!({"action": "get", "task_id": task_id, "path": "alias.txt"}),
    );
    assert_eq!(by_task_id_art, by_id_art);
    assert_eq!(by_task_id_art["content"], "alias artifact bytes\n");

    // 5. Differing `id` and `task_id` are refused, naming both.
    let other_id = "ORB-99999999";
    for (tool, input) in [
        (
            "orbit_task_show",
            json!({"id": task_id, "task_id": other_id}),
        ),
        (
            "orbit_task_update",
            json!({"id": task_id, "task_id": other_id, "priority": "low"}),
        ),
        (
            "orbit_task_update",
            json!({
                "workspace": workspace.work,
                "request_id": "req-alias-conflict",
                "expected_revision": revision,
                "id": task_id,
                "task_id": other_id,
                "comment": "conflict",
            }),
        ),
        (
            "orbit_task_artifact",
            json!({"action": "get", "id": task_id, "task_id": other_id, "path": "alias.txt"}),
        ),
        (
            "orbit_task_artifact",
            json!({"action": "put", "id": task_id, "task_id": other_id, "source_path": source, "path": "alias.txt"}),
        ),
    ] {
        let error = client.call_tool_err(tool, input);
        assert_eq!(error["code"], "invalid_input", "{tool}: {error}");
        let message = error["message"].as_str().expect("error message");
        assert!(
            message.contains("id") && message.contains("task_id"),
            "{tool}: conflict refusal must name both `id` and `task_id`: {message}"
        );
    }

    // 6. orbit_task_eligible declares no `id`, so `task_id` remains refused.
    let eligible_err = client.call_tool_err("orbit_task_eligible", json!({"task_id": task_id}));
    assert_eq!(eligible_err["code"], "invalid_input", "{eligible_err}");
}
