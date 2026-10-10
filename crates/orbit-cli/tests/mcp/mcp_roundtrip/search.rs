//! The six-term duplicate-check regression through CLI, tool dispatch and MCP.
use super::*;

#[test]
fn multi_word_search_ranks_partial_hits_and_preserves_cli_tool_mcp_parity() {
    let workspace = McpWorkspace::init();
    let mut client = workspace.serve();
    let query = "generation participant mcp serve pinned deploy";
    // No task contains all six terms. Two chunks in the same task must not
    // yield two results, and title order must not override coverage or BM25.
    for (title, description, tags) in [
        (
            "generation participant mcp padding padding padding",
            "Unrelated fixture",
            vec![],
        ),
        (
            "generation participant mcp",
            "participant",
            vec!["relevant"],
        ),
        (
            "generation participant mcp serve pinned",
            "Unrelated fixture",
            vec!["relevant"],
        ),
        ("deploy", "Unrelated fixture", vec!["relevant"]),
        (
            "generationless participantish",
            "Unrelated fixture",
            vec!["relevant"],
        ),
    ] {
        client.call_tool_ok("orbit_task_add", json!({
            "title": title, "description": description, "tags": tags, "complexity": "low", "model": "codex"
        }));
    }
    let cli = |kind: &str, query: &str, limit: usize, tag: Option<&str>| {
        let limit = limit.to_string();
        let mut args = vec!["search", query, "--kind", kind, "--limit", &limit, "--json"];
        if let Some(tag) = tag {
            args.extend(["--tag", tag]);
        }
        let output =
            orbit_ok(McpWorkspace::orbit_command(&workspace.work, &workspace.home).args(args));
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let response = cli("task", query, 10, None);
    let results = response["results"].as_array().unwrap();
    assert_eq!(
        results
            .iter()
            .map(|hit| hit["title"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "generation participant mcp serve pinned",
            "generation participant mcp",
            "generation participant mcp padding padding padding",
            "deploy"
        ]
    );
    assert_eq!(
        results
            .iter()
            .map(|hit| hit["matched_by"].clone())
            .collect::<Vec<_>>(),
        vec![
            json!(["partial", "terms:5/6"]),
            json!(["partial", "terms:3/6"]),
            json!(["partial", "terms:3/6"]),
            json!(["partial", "terms:1/6"])
        ]
    );
    assert!(!response["notes"].as_array().unwrap().is_empty());
    let input = json!({"query":query, "kind":"task", "limit":10, "model":"codex"});
    assert_eq!(client.call_tool_ok("orbit_search", input.clone()), response);
    let output = orbit_ok(
        McpWorkspace::orbit_command(&workspace.work, &workspace.home).args([
            "tool",
            "run",
            "orbit.search",
            "--input",
            &input.to_string(),
        ]),
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        response
    );
    let plain = orbit_ok(
        McpWorkspace::orbit_command(&workspace.work, &workspace.home)
            .args(["search", query, "--kind", "task"]),
    );
    let plain = String::from_utf8_lossy(&plain.stdout);
    assert!(
        plain.contains("partial") && plain.contains("terms:5/6"),
        "partial labels reach the CLI MATCH column: {plain}"
    );

    let full = client.call_tool_ok(
        "orbit_task_add",
        json!({
            "title":query, "description":"Unrelated fixture", "complexity":"low", "model":"codex"
        }),
    );
    let response = cli("task", query, 3, None);
    assert_eq!(response["results"][0]["id"], full["id"]);
    assert!(response["results"][0].get("matched_by").is_none());
    assert_eq!(
        response["results"][1]["matched_by"],
        json!(["partial", "terms:5/6"])
    );
    let limited = cli("task", query, 1, None);
    assert_eq!(limited["results"].as_array().unwrap().len(), 1);
    assert!(limited["notes"].as_array().unwrap().is_empty());
    let filtered = cli("task", query, 2, Some("relevant"));
    assert_eq!(filtered["results"].as_array().unwrap().len(), 2);
    assert_eq!(
        filtered["results"][0]["matched_by"],
        json!(["partial", "terms:5/6"])
    );

    client.call_tool_ok(
        "orbit_friction_add",
        json!({
            "title": "generation caution participant", "body": "Substring fixture", "model": "codex"
        }),
    );
    assert!(
        cli("friction", "generation participant", 10, None)["results"]
            .as_array()
            .unwrap()
            .is_empty(),
        "friction matching does not use the task any-term fallback"
    );
    assert_eq!(
        cli("friction", "generation", 10, None)["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Exercise runtime notes, not copies of their source text. Distinguish
    // each branch's matching contract without pinning the full prose.
    for kind in ["task", "friction", "all"] {
        let empty = cli(kind, "absentword absentphrase", 10, None);
        assert!(empty["results"].as_array().unwrap().is_empty());
        let note = empty["notes"][0].as_str().unwrap();
        assert_eq!(note.contains("task search"), kind != "friction");
        assert_eq!(note.contains("friction search"), kind != "task");
        if kind != "friction" {
            assert!(note.contains("all terms") && note.contains("any term"));
        }
        if kind != "task" {
            assert!(note.contains("substring") && note.contains("adjacent"));
        }
        assert_eq!(
            client.call_tool_ok(
                "orbit_search",
                json!({"query":"absentword absentphrase", "kind":kind, "limit":10, "model":"codex"})
            ),
            empty
        );
    }
}
