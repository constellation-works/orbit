use super::*;

#[test]
fn desktop_writes_reconcile_after_restart_and_reject_stale_or_implicit_destinations() {
    let workspace = McpWorkspace::init();
    let selector = workspace.work.to_str().unwrap();
    let mut client = workspace.serve();
    let create = json!({"workspace":selector,"model":"codex","request_id":"desktop-create-proof","operation":{"kind":"create","title":"Desktop capture","description":"Durable evidence","acceptance_criteria":["One effect across restart"]}});
    let created = client.call_tool_ok("orbit_desktop_task_write", create.clone());
    let id = created["snapshot"]["task"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(id.starts_with("TST-"), "public task ID: {created}");
    assert_eq!(created["workspace"], selector);
    assert_eq!(created["snapshot"]["task"]["status"], "proposed");
    drop(client);
    let mut client = workspace.serve();
    let replay = client.call_tool_ok("orbit_desktop_task_write", create.clone());
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["snapshot"]["task"]["id"], id);
    let mut changed = create;
    changed["operation"]["title"] = json!("Changed retry");
    assert_eq!(
        client.call_tool_ok("orbit_desktop_task_write", changed)["mutation_applied"],
        false
    );
    let comment = json!({"workspace":selector,"model":"codex","request_id":"desktop-comment-proof","operation":{"kind":"comment","id":id,"expected_revision":replay["snapshot"]["revision"],"comment":"Recorded once"}});
    let first = client.call_tool_ok("orbit_desktop_task_write", comment.clone());
    drop(client);
    let mut client = workspace.serve();
    let again = client.call_tool_ok("orbit_desktop_task_write", comment);
    assert_eq!(again["replayed"], true);
    assert_eq!(
        again["snapshot"]["comments_total"],
        first["snapshot"]["comments_total"]
    );
    let stale = client.call_tool_ok("orbit_desktop_task_write", json!({"workspace":selector,"request_id":"desktop-stale-proof","operation":{"kind":"edit","id":id,"expected_revision":replay["snapshot"]["revision"],"fields":{"title":"Stale edit"}}}));
    assert_eq!(stale["conflict"]["code"], "revision_conflict");
    assert_eq!(stale["snapshot"]["task"]["title"], "Desktop capture");
    let snapshot = client.call_tool_ok(
        "orbit_desktop_task_snapshot",
        json!({"workspace":selector,"id":id}),
    );
    assert_eq!(snapshot["revision"], again["snapshot"]["revision"]);
    assert_eq!(snapshot["actions"]["complete"]["enabled"], false);
    let list = client.call_tool_ok("orbit_desktop_read", json!({"workspace":selector,"scope":"tasks","search":"Desktop","status":"proposed","limit":1}));
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["id"], id);
    for tool in [
        "orbit_desktop_read",
        "orbit_desktop_task_snapshot",
        "orbit_desktop_task_write",
    ] {
        assert_eq!(
            client.call_tool_err(tool, json!({"id":id,"scope":"tasks"}))["code"],
            "invalid_input"
        );
    }
    assert_eq!(
        client.call_tool_err(
            "orbit_desktop_read",
            json!({"workspace":selector,"scope":"runs"})
        )["code"],
        "capability_denied"
    );
    let mut federated = federated_client(&workspace);
    let discovered = federated.call_tool_ok("orbit_workspace_list", json!({}));
    let qualified = discovered["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "mcp-roundtrip")
        .unwrap()["selector"]
        .clone();
    assert!(qualified.is_string());
    let snapshot = federated.call_tool_ok(
        "orbit_desktop_task_snapshot",
        json!({"workspace":qualified,"id":id}),
    );
    assert_eq!(snapshot["workspace"], qualified);
    assert_eq!(snapshot["task"]["id"], id);
    federated.call_tool_err(
        "orbit_desktop_task_snapshot",
        json!({"workspace":"ws_mcp-roundtrip","id":id}),
    );
}
