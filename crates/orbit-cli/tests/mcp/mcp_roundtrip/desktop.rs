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

/// [ORB-14173] A replica's routine projection matches what it can actually
/// change: an operator lists and enables its worktree GC, while owner work —
/// ship sweep, auto-task minting and toggles — is refused with the owner
/// named and leaves every definition byte-identical.
#[test]
fn replica_routine_control_enables_only_worktree_gc_and_names_the_owner_for_the_rest() {
    let workspace = McpWorkspace::init_replica_of("hm_remote_owner");
    let selector = workspace.work.to_str().unwrap();
    let routines = workspace.work.join(".orbit/routines");
    let gc_path = routines.join("worktree_gc.yaml");
    let ship_path = routines.join("ship_sweep.yaml");
    let auto_task_path = workspace.work.join(".orbit/auto_tasks/qa-sweep.yaml");
    let ship_before = std::fs::read(&ship_path).unwrap();
    let auto_task_before = std::fs::read(&auto_task_path).unwrap();
    let mut client = workspace.serve_with_args(&["--operator"]);

    let listed = client.call_tool_ok(
        "orbit_routine_control",
        json!({"workspace":selector,"action":"list","limit":50}),
    );
    let row = |name: &str| {
        listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["name"] == name)
            .unwrap_or_else(|| panic!("{name} not listed: {listed}"))
            .clone()
    };
    let gc = row("worktree-gc-mcp-roundtrip");
    assert_eq!(gc["toggle_available"], true, "{gc}");
    assert_eq!(gc["enabled"], false, "{gc}");
    let ship = row("ship-sweep-mcp-roundtrip");
    assert_eq!(ship["toggle_available"], false, "{ship}");
    assert_eq!(ship["state"], "owner_only", "{ship}");
    assert!(
        ship["toggle_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("hm_remote_owner")),
        "an unavailable toggle explains owner authority: {ship}"
    );

    let enabled = client.call_tool_ok(
        "orbit_routine_control",
        json!({"workspace":selector,"action":"toggle","name":"worktree-gc-mcp-roundtrip",
            "target":"job:worktree_gc_pipeline","expected_enabled":false,"enabled":true}),
    );
    assert_eq!(enabled["enabled"], true, "{enabled}");
    assert!(
        std::fs::read_to_string(&gc_path)
            .unwrap()
            .contains("enabled: true"),
        "the replica GC definition was enabled"
    );

    let refused = client.call_tool_err(
        "orbit_routine_control",
        json!({"workspace":selector,"action":"toggle","name":"ship-sweep-mcp-roundtrip",
            "target":"job:workspace_ship_pipeline","expected_enabled":false,"enabled":true}),
    );
    assert_eq!(refused["code"], "capability_refused", "{refused}");
    assert!(
        refused["message"]
            .as_str()
            .is_some_and(|message| message.contains("hm_remote_owner")),
        "{refused}"
    );
    for (tool, input) in [
        (
            "orbit_auto_task_mint",
            json!({"workspace":selector,"name":"qa-sweep","acknowledge_unconditional":true}),
        ),
        (
            "orbit_auto_task_update",
            json!({"workspace":selector,"name":"qa-sweep","expected_enabled":false,"enabled":true}),
        ),
        (
            "orbit_task_add",
            json!({"workspace":selector,"title":"must not fork","description":"replica","complexity":"low","model":"codex"}),
        ),
    ] {
        let refused = client.call_tool_err(tool, input);
        assert_eq!(refused["code"], "capability_refused", "{tool}: {refused}");
    }
    assert_eq!(std::fs::read(&ship_path).unwrap(), ship_before);
    assert_eq!(std::fs::read(&auto_task_path).unwrap(), auto_task_before);

    // The enabled replica GC is the host clock's to fire: the host listing
    // schedules it, and keeps the ship sweep as the owner's.
    drop(client);
    let listed = orbit_ok(
        McpWorkspace::orbit_command(&workspace.work, &workspace.home)
            .args(["routine", "list", "--json"]),
    );
    let listed: Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert!(
        listed["routines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["name"] == "worktree-gc-mcp-roundtrip" && row["effective"] == true),
        "{listed}"
    );
    assert!(
        listed["owner_only"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["name"] == "ship-sweep-mcp-roundtrip"),
        "{listed}"
    );
}

#[test]
fn desktop_governed_status_approval_and_crew_preserve_receipts_and_review_gates() {
    let workspace = McpWorkspace::init();
    let selector = workspace.work.to_str().unwrap();
    let mut client = workspace.serve();
    let created = client.call_tool_ok(
        "orbit_task_add",
        json!({"workspace":selector,"model":"codex",
        "request_id":"status-create","title":"Row controls","description":"Governed writes",
        "acceptance_criteria":["No duplicate approval"]}),
    );
    let id = created["snapshot"]["task"]["id"].as_str().unwrap();
    let revision = &created["snapshot"]["revision"];
    let approval = json!({"workspace":selector,"model":"codex","id":id,
        "request_id":"approve","expected_revision":revision,"status":"backlog"});
    for patch in [
        json!({"status":"done"}),
        json!({"status":"review"}),
        json!({"title":"Combined approval"}),
        json!({"crew":""}),
    ] {
        let mut refused = approval.clone();
        refused
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        let result = client.call_tool_ok("orbit_task_update", refused);
        assert_eq!(result["mutation_applied"], false, "{result}");
    }
    let mut implicit = approval.clone();
    implicit.as_object_mut().unwrap().remove("workspace");
    assert_eq!(
        client.call_tool_err("orbit_task_update", implicit)["code"],
        "invalid_input"
    );
    let mut wrong = approval.clone();
    wrong["workspace"] = json!("unknown-destination");
    client.call_tool_err("orbit_task_update", wrong);
    let first = client.call_tool_ok("orbit_task_update", approval.clone());
    assert_eq!(first["snapshot"]["task"]["status"], "backlog");
    assert_eq!(
        first["snapshot"]["history"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|h| h["event"] == "proposal_approved")
            .count(),
        1
    );
    // Throw away the receipt and restart the transport; resubmission still has
    // the old observed revision and must recover the durable accepted write.
    drop(client);
    let mut client = workspace.serve();
    let replay = client.call_tool_ok("orbit_task_update", approval.clone());
    assert_eq!(replay["replayed"], true);
    assert_eq!(
        replay["snapshot"]["revision"],
        first["snapshot"]["revision"]
    );
    let mut changed = approval.clone();
    changed["status"] = json!("blocked");
    assert_eq!(
        client.call_tool_ok("orbit_task_update", changed)["mutation_applied"],
        false
    );
    let mut stale = approval;
    stale["request_id"] = json!("stale");
    stale["status"] = json!("blocked");
    assert_eq!(
        client.call_tool_ok("orbit_task_update", stale)["conflict"]["code"],
        "revision_conflict"
    );
    // A long-lived client mixes ordinary and guarded calls on the same session.
    client.call_tool_ok(
        "orbit_task_update",
        json!({"workspace":selector,"id":id,"model":"codex","title":"Ordinary caller"}),
    );
    let snap = client.call_tool_ok(
        "orbit_task_show",
        json!({"workspace":selector,"id":id,"snapshot":true}),
    );
    let crew = snap["task"]["crew"].as_str().unwrap().to_string();
    let edited = client.call_tool_ok(
        "orbit_task_update",
        json!({"workspace":selector,"id":id,"model":"codex",
        "request_id":"crew","expected_revision":snap["revision"],"crew":crew,"status":"blocked"}),
    );
    assert_eq!(edited["snapshot"]["task"]["status"], "blocked");
    assert_eq!(edited["snapshot"]["task"]["title"], "Ordinary caller");
    let defaulted = client.call_tool_ok(
        "orbit_task_update",
        json!({"workspace":selector,"id":id,"model":"codex",
        "request_id":"default-crew","expected_revision":edited["snapshot"]["revision"],"crew":""}),
    );
    assert!(defaulted["snapshot"].is_object(), "{defaulted}");
    orbit_ok(
        McpWorkspace::orbit_command(&workspace.work, &workspace.home)
            .args(["task", "update", id, "--status", "review", "--force"]),
    );
    let snap = client.call_tool_ok(
        "orbit_task_show",
        json!({"workspace":selector,"id":id,"snapshot":true}),
    );
    assert!(
        snap["actions"]["status"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["status"] == "done" && a["enabled"] == false)
    );
    assert_eq!(
        snap["actions"]["complete"]["enabled"], false,
        "an ordinary client has no completion authority"
    );
    let done = json!({"workspace":selector,"id":id,"model":"codex","request_id":"bypass",
        "expected_revision":snap["revision"],"status":"done"});
    assert_eq!(
        client.call_tool_ok("orbit_task_update", done.clone())["mutation_applied"],
        false
    );
    let mut operator = workspace.serve_with_args(&["--operator"]);
    assert_eq!(
        operator.call_tool_ok("orbit_task_update", done)["mutation_applied"],
        false,
        "operator authority also cannot bypass the evidence-bound review operation"
    );
    let mut forced = json!({"workspace":selector,"id":id,"model":"codex","request_id":"force",
        "expected_revision":snap["revision"],"status":"proposed","force":true});
    client.call_tool_err("orbit_task_update", forced.clone());
    forced.as_object_mut().unwrap().remove("force");
    assert_eq!(
        client.call_tool_ok("orbit_task_update", forced)["mutation_applied"],
        false
    );
    assert_eq!(
        client.call_tool_ok(
            "orbit_task_show",
            json!({"workspace":selector,"id":id,"snapshot":true})
        )["revision"],
        snap["revision"]
    );
}
