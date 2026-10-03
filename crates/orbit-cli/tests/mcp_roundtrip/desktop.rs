use super::*;

#[test]
fn desktop_task_status_sets_compose_with_search_priority_and_pagination() {
    let workspace = McpWorkspace::init();
    let selector = workspace.work.to_str().unwrap();
    let mut client = workspace.serve();
    for (index, (status, title, priority)) in [
        ("in-progress", "Needle running", "medium"),
        ("review", "Needle review", "medium"),
        ("blocked", "Needle blocked", "medium"),
        ("backlog", "Needle queued", "medium"),
        ("proposed", "Needle proposed", "medium"),
        ("done", "Needle completed", "medium"),
        ("backlog", "Other title", "medium"),
        ("blocked", "Needle urgent", "high"),
    ]
    .into_iter()
    .enumerate()
    {
        let created = client.call_tool_ok(
            "orbit_task_add",
            json!({"workspace":selector,"model":"codex","request_id":format!("filter-{index}"),"title":title,"description":"Filter fixture","acceptance_criteria":["Visible when included"],"priority":priority}),
        );
        let id = created["snapshot"]["task"]["id"].as_str().unwrap();
        if status != "proposed" {
            // Human override seeds lifecycle states only in the disposable
            // child-process fixture; no task is dispatched or completed by MCP.
            orbit_ok(
                McpWorkspace::orbit_command(&workspace.work, &workspace.home)
                    .args(["task", "update", id, "--status", status, "--force"]),
            );
        }
    }
    let mut input = json!({"workspace":selector,"view":"bounded","status":"in-progress,review,blocked,backlog","search":"nEeDlE","priority":"medium","limit":2});
    let first = client.call_tool_ok("orbit_task_list", input.clone());
    assert_eq!(first["total"], 4);
    assert_eq!(first["pagination"]["next_offset"], 2);
    let mut ids = BTreeSet::new();
    for offset in [0, 2] {
        input["offset"] = json!(offset);
        let page = client.call_tool_ok("orbit_task_list", input.clone());
        assert_eq!(page["total"], 4);
        assert_eq!(page["items"].as_array().unwrap().len(), 2);
        for task in page["items"].as_array().unwrap() {
            let status = task["status"].as_str().unwrap().replace('_', "-");
            assert!(
                ["in-progress", "review", "blocked", "backlog"].contains(&status.as_str()),
                "only included lifecycle states appear: {task}"
            );
            assert!(ids.insert(task["id"].as_str().unwrap().to_string()));
        }
        if offset == 2 {
            assert!(page["pagination"]["next_offset"].is_null());
        }
    }
    input["offset"] = json!(0);
    input["status"] = json!(["proposed", "done"]);
    let expanded = client.call_tool_ok("orbit_task_list", input.clone());
    assert_eq!(expanded["total"], 2);
    let statuses: BTreeSet<_> = expanded["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|task| task["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, BTreeSet::from(["proposed", "done"]));
    input["status"] = json!("review");
    assert_eq!(
        client.call_tool_ok("orbit_task_list", input.clone())["total"],
        1
    );
    input["status"] = json!("review,not-a-status");
    assert_eq!(
        client.call_tool_err("orbit_task_list", input.clone())["code"],
        "invalid_input"
    );
    input.as_object_mut().unwrap().remove("status");
    assert_eq!(
        client.call_tool_ok("orbit_task_list", input.clone())["total"],
        6
    );
    input["status"] = Value::Null;
    assert_eq!(
        client.call_tool_ok("orbit_task_list", input.clone())["total"],
        6,
        "a null status retains the bounded reader's unfiltered contract"
    );
    input.as_object_mut().unwrap().remove("status");
    input["priority"] = json!("high");
    assert_eq!(client.call_tool_ok("orbit_task_list", input)["total"], 1);
}

#[test]
fn desktop_writes_reconcile_after_restart_and_reject_stale_or_implicit_destinations() {
    let workspace = McpWorkspace::init();
    let selector = workspace.work.to_str().unwrap();
    let mut client = workspace.serve();
    let create = json!({"workspace":selector,"model":"codex","request_id":"desktop-create-proof","title":"Desktop capture","description":"Durable evidence","acceptance_criteria":["One effect across restart"]});
    let created = client.call_tool_ok("orbit_task_add", create.clone());
    let id = created["snapshot"]["task"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(id.starts_with("TST-"), "public task ID: {created}");
    assert_eq!(created["workspace"], selector);
    assert_eq!(created["snapshot"]["task"]["status"], "proposed");
    drop(client);
    let mut client = workspace.serve();
    let replay = client.call_tool_ok("orbit_task_add", create.clone());
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["snapshot"]["task"]["id"], id);
    let mut changed = create;
    changed["title"] = json!("Changed retry");
    assert_eq!(
        client.call_tool_ok("orbit_task_add", changed)["mutation_applied"],
        false
    );
    let comment = json!({"workspace":selector,"model":"codex","request_id":"desktop-comment-proof","id":id,"expected_revision":replay["snapshot"]["revision"],"comment":"Recorded once"});
    let first = client.call_tool_ok("orbit_task_update", comment.clone());
    drop(client);
    let mut client = workspace.serve();
    let again = client.call_tool_ok("orbit_task_update", comment);
    assert_eq!(again["replayed"], true);
    assert_eq!(
        again["snapshot"]["comments_total"],
        first["snapshot"]["comments_total"]
    );
    // Both cached advertised names and canonical retired names are unavailable.
    for name in [
        "orbit_desktop_read",
        "orbit_desktop_task_snapshot",
        "orbit_desktop_task_write",
        "orbit_desktop_drain",
        "orbit_desktop_automation",
        "orbit.desktop.read",
        "orbit.desktop.task.snapshot",
        "orbit.desktop.task.write",
        "orbit.desktop.drain",
        "orbit.desktop.automation",
    ] {
        let error = client.call_tool_err(name, json!({"workspace":selector}));
        assert_eq!(error["code"], "tool_not_found", "{name}: {error}");
    }
    let stale = client.call_tool_ok("orbit_task_update", json!({"workspace":selector,"request_id":"desktop-stale-proof","id":id,"expected_revision":replay["snapshot"]["revision"],"title":"Stale edit"}));
    assert_eq!(stale["conflict"]["code"], "revision_conflict");
    assert_eq!(stale["snapshot"]["task"]["title"], "Desktop capture");
    let snapshot = client.call_tool_ok(
        "orbit_task_show",
        json!({"workspace":selector,"id":id,"snapshot":true}),
    );
    assert_eq!(snapshot["revision"], again["snapshot"]["revision"]);
    assert_eq!(snapshot["actions"]["complete"]["enabled"], false);
    let list = client.call_tool_ok("orbit_task_list", json!({"workspace":selector,"view":"bounded","search":"Desktop","status":"proposed","limit":1}));
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["id"], id);
    for (tool, input) in [
        ("orbit_task_list", json!({"view":"bounded"})),
        ("orbit_task_show", json!({"id":id,"snapshot":true})),
        (
            "orbit_task_update",
            json!({"id":id,"request_id":"implicit","expected_revision":"seen","comment":"proof"}),
        ),
    ] {
        assert_eq!(client.call_tool_err(tool, input)["code"], "invalid_input");
    }
    assert_eq!(
        client.call_tool_err(
            "orbit_workflow_run_list",
            json!({"workspace":selector,"view":"bounded"})
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
        "orbit_task_show",
        json!({"workspace":qualified,"id":id,"snapshot":true}),
    );
    assert_eq!(snapshot["workspace"], qualified);
    assert_eq!(snapshot["task"]["id"], id);
    federated.call_tool_err(
        "orbit_task_show",
        json!({"workspace":"ws_mcp-roundtrip","id":id,"snapshot":true}),
    );
}

#[test]
fn domain_automation_stdio_preserves_observed_routine_state_and_refuses_dispatch_without_authority()
{
    let workspace = McpWorkspace::init();
    let selector = workspace.work.to_str().unwrap();
    let global = workspace.home.join(".orbit");
    let jobs = global.join("resources/jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    std::fs::write(jobs.join("stdio-maintenance.yaml"), "schemaVersion: 2\nkind: Job\nmetadata:\n  name: stdio-maintenance\nspec:\n  state: disabled\n  kind: workflow\n  max_active_runs: 1\n  steps:\n    - id: nap\n      spec:\n        type: deterministic\n        action: sleep\n        config: {}\n").unwrap();
    let routines = workspace.work.join(".orbit/routines");
    std::fs::create_dir_all(&routines).unwrap();
    let routine_path = routines.join("stdio-routine.yaml");
    std::fs::write(&routine_path,"schemaVersion: 1\nname: stdio-routine\nenabled: true\ntrigger:\n  cron: '0 9 * * *'\ntarget: job:stdio-maintenance\n").unwrap();
    let mut client = workspace.serve_with_args(&["--operator"]);
    let listed = client.call_tool_ok(
        "orbit_routine_control",
        json!({"workspace":selector,"action":"list"}),
    );
    assert!(
        listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["name"] == "stdio-routine" && item["enabled"] == true),
        "{listed}"
    );
    let toggle = json!({"workspace":selector,"action":"toggle","name":"stdio-routine","target":"job:stdio-maintenance","expected_enabled":true,"enabled":false});
    client.call_tool_ok("orbit_routine_control", toggle.clone());
    let after_toggle = std::fs::read(&routine_path).unwrap();
    let conflict = client.call_tool_err("orbit_routine_control", toggle);
    assert_eq!(conflict["code"], "invalid_input", "{conflict}");
    assert_eq!(std::fs::read(&routine_path).unwrap(), after_toggle);
    drop(client);
    let mut client = workspace.serve_with_args(&["--operator"]);
    let reopened = client.call_tool_ok(
        "orbit_routine_control",
        json!({"workspace":selector,"action":"list"}),
    );
    assert!(
        reopened["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["name"] == "stdio-routine" && item["enabled"] == false),
        "{reopened}"
    );
    let readiness = client.call_tool_ok(
        "orbit_workflow_auto",
        json!({"workspace":selector,"action":"status"}),
    );
    assert_eq!(readiness["workspace"], selector);
    let catalog = client.call_tool_ok(
        "orbit_workflow_run_list",
        json!({"workspace":selector,"view":"bounded","include_catalog":true}),
    );
    assert!(
        catalog["catalog"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["name"] == "stdio-maintenance" && item["state"] == "disabled"),
        "{catalog}"
    );
    assert_eq!(catalog["runs"]["total"], 0, "observation does not dispatch");
    client.call_tool_err(
        "orbit_pipeline_invoke",
        json!({"workspace":selector,"job_name":"stdio-maintenance","default_input":true}),
    );
    client.call_tool_err("orbit_pipeline_invoke",json!({"workspace":selector,"job_name":"stdio-maintenance","default_input":true,"input":{}}));
    client.call_tool_err(
        "orbit_workflow_run_resume",
        json!({"workspace":selector,"id":"jrun-missing"}),
    );
    client.call_tool_err(
        "orbit_workflow_auto",
        json!({"workspace":selector,"action":"resize","id":"jrun-missing","concurrency":1}),
    );
    let runs = client.call_tool_ok(
        "orbit_workflow_run_list",
        json!({"workspace":selector,"view":"bounded"}),
    );
    assert_eq!(
        runs["total"], 0,
        "disabled and mixed-input refusals do not submit runs"
    );
    drop(client);
    let mut unprivileged = workspace.serve();
    for (tool, input) in [
        (
            "orbit_routine_control",
            json!({"workspace":selector,"action":"list"}),
        ),
        (
            "orbit_workflow_auto",
            json!({"workspace":selector,"action":"status"}),
        ),
        (
            "orbit_pipeline_invoke",
            json!({"workspace":selector,"job_name":"stdio-maintenance","input":{}}),
        ),
        (
            "orbit_workflow_run_resume",
            json!({"workspace":selector,"id":"jrun-missing"}),
        ),
        (
            "orbit_workflow_auto",
            json!({"workspace":selector,"action":"resize","id":"jrun-missing","concurrency":1}),
        ),
    ] {
        assert_eq!(
            unprivileged.call_tool_err(tool, input)["code"],
            "capability_denied",
            "{tool}"
        );
    }
    assert_eq!(std::fs::read(&routine_path).unwrap(), after_toggle);
}
