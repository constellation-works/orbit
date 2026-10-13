//! Task add/update pre-flight is one core operation, so `orbit task add` /
//! `orbit task update` and the `orbit.task.add` / `orbit.task.update` tools
//! return the same warnings for the same write. Warnings are identified by
//! the record they name, never by their wording.

use serde_json::{Value, json};

use crate::isolated_cli_fixture::Fixture;

/// A registered tool an operator disabled: still a valid requirement, which
/// the add keeps and warns about.
const DISABLED_TOOL: &str = "orbit.search";

fn warnings(document: &Value) -> Vec<String> {
    document
        .get("warnings")
        .map(|warnings| {
            warnings
                .as_array()
                .unwrap_or_else(|| panic!("warnings array in {document}"))
                .iter()
                .map(|warning| warning.as_str().expect("warning text").to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn assert_names(warnings: &[String], named: &str, document: &Value) {
    assert!(
        warnings.iter().any(|warning| warning.contains(named)),
        "a warning names `{named}`: {document}"
    );
}

fn tool(fixture: &Fixture, name: &str, input: Value) -> Value {
    fixture.json(&["tool", "run", name, "--input", &input.to_string()])
}

fn add_proposed(fixture: &Fixture, title: &str) -> String {
    let created = fixture.json(&[
        "task",
        "add",
        "--title",
        title,
        "--complexity",
        "low",
        "--json",
    ]);
    assert!(created.get("warnings").is_none(), "{created}");
    created["id"].as_str().expect("task id").to_string()
}

fn add_warnings_match_on_cli_and_tool() {
    let fixture = Fixture::new();
    fixture.json(&["tool", "disable", DISABLED_TOOL, "--format", "json"]);

    // A non-chore task with no context_files that requires a disabled tool.
    let cli = fixture.json(&[
        "task",
        "add",
        "--title",
        "Feature filed without scope",
        "--description",
        "Exercises the add pre-flight.",
        "--complexity",
        "low",
        "--type",
        "feature",
        "--required-tools",
        DISABLED_TOOL,
        "--json",
    ]);
    let via_tool = tool(
        &fixture,
        "orbit.task.add",
        json!({
            "title": "Feature filed without scope",
            "description": "Exercises the add pre-flight.",
            "complexity": "low",
            "type": "feature",
            "required_tools": [DISABLED_TOOL],
            "workspace": "audit-qa",
        }),
    );
    let cli_warnings = warnings(&cli);
    assert_eq!(cli_warnings.len(), 2, "{cli}");
    assert_names(&cli_warnings, DISABLED_TOOL, &cli);
    assert_names(&cli_warnings, "context_files", &cli);
    assert_eq!(warnings(&via_tool), cli_warnings, "{via_tool}");

    // A parent and a dependency this host cannot read are stored and warned
    // about. The tool takes neither field on add.
    let cli = fixture.json(&[
        "task",
        "add",
        "--title",
        "Subtask of a foreign parent",
        "--complexity",
        "low",
        "--parent",
        "EXT-1",
        "--dependencies",
        "EXT-2",
        "--json",
    ]);
    let cli_warnings = warnings(&cli);
    assert_eq!(cli_warnings.len(), 2, "{cli}");
    assert_names(&cli_warnings, "EXT-1", &cli);
    assert_names(&cli_warnings, "EXT-2", &cli);
}

fn update_and_approval_warnings_match_on_cli_and_tool() {
    let fixture = Fixture::new();
    let [cli_id, tool_id] = ["CLI twin", "Tool twin"].map(|title| add_proposed(&fixture, title));

    let cli = fixture.json(&[
        "task",
        "update",
        &cli_id,
        "--dependencies",
        "EXT-3",
        "--json",
    ]);
    let via_tool = tool(
        &fixture,
        "orbit.task.update",
        json!({ "id": tool_id, "dependencies": ["EXT-3"] }),
    );
    let cli_warnings = warnings(&cli);
    assert_eq!(cli_warnings.len(), 1, "{cli}");
    assert_names(&cli_warnings, "EXT-3", &cli);
    assert_eq!(warnings(&via_tool), cli_warnings, "{via_tool}");

    let blocker = "Await operator credentials before executing the repair";
    let receipt = format!(
        "operation_id={}\n{}",
        "a".repeat(64),
        json!({"assessment": {"blocked_by": [blocker]}})
    );
    for id in [&cli_id, &tool_id] {
        fixture
            .command(&["task", "update", id, "--comment", &receipt, "--json"])
            .env("ORBIT_ACTOR", "task-pilot")
            .assert()
            .success();
    }
    let cli = fixture.json(&["task", "update", &cli_id, "--approve", "--json"]);
    let via_tool = tool(
        &fixture,
        "orbit.task.update",
        json!({ "id": tool_id, "status": "backlog" }),
    );
    assert_eq!(cli["status"], json!("backlog"), "{cli}");
    assert_eq!(via_tool["status"], json!("backlog"), "{via_tool}");
    let cli_warnings = warnings(&cli);
    assert_names(&cli_warnings, blocker, &cli);
    assert_eq!(warnings(&via_tool), cli_warnings, "{via_tool}");
}

#[test]
fn write_preflight_cli_tool_parity() {
    for (name, check) in [
        (
            "add_warnings_match_on_cli_and_tool",
            add_warnings_match_on_cli_and_tool as fn(),
        ),
        (
            "update_and_approval_warnings_match_on_cli_and_tool",
            update_and_approval_warnings_match_on_cli_and_tool as fn(),
        ),
    ] {
        tracing::info!(case = name, "CLI contract row");
        check();
    }
}
