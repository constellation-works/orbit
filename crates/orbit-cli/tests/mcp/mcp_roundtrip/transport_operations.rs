//! Positive production-stdio proofs with disposable records and deterministic jobs.
use super::*;

#[test]
fn stdio_artifact_put_reopens_intact_bytes_and_refuses_workspace_escape() {
    let workspace = McpWorkspace::init();
    let selector = workspace.work.to_str().unwrap();
    let mut client = workspace.serve();
    let task = client.call_tool_ok("orbit_task_add", json!({"title":"Artifact wire proof","description":"Disposable evidence","complexity":"low","model":"codex"}));
    let id = task["id"].as_str().unwrap();
    let source = workspace.work.join(".orbit/tmp/source.txt");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, "Exact evidence\nπ\n").unwrap();
    client.call_tool_ok("orbit_task_artifact_put",json!({"workspace":selector,"id":id,"source_path":source,"path":"evidence/report.txt","model":"codex"}));
    drop(client);
    let mut client = workspace.serve();
    let read = client.call_tool_ok(
        "orbit_task_artifact_get",
        json!({"workspace":selector,"id":id,"path":"evidence/report.txt"}),
    );
    assert_eq!(read["content"], "Exact evidence\nπ\n");
    assert_eq!(read["encoding"], "utf8");
    let before = client.call_tool_ok("orbit_task_show", json!({"id":id,"fields":["artifacts"]}));
    let outside = workspace.home.join("outside.txt");
    std::fs::write(&outside, "must not be attached").unwrap();
    client.call_tool_err("orbit_task_artifact_put",json!({"workspace":selector,"id":id,"source_path":outside,"path":"escape.txt","model":"codex"}));
    client.call_tool_err("orbit_task_artifact_put",json!({"workspace":selector,"id":id,"source_path":source,"path":"../escape.txt","model":"codex"}));
    client.call_tool_err("orbit_task_artifact_put",json!({"workspace":"ws_missing","id":id,"source_path":source,"path":"wrong-workspace.txt","model":"codex"}));
    assert_eq!(
        client.call_tool_ok("orbit_task_show", json!({"id":id,"fields":["artifacts"]})),
        before
    );
    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        "must not be attached"
    );
}

#[test]
fn stdio_auto_task_crud_mints_without_dispatch_and_reopens_definition_state() {
    let workspace = McpWorkspace::init();
    let selector = workspace.work.to_str().unwrap();
    let mut client = workspace.serve_with_args(&["--operator"]);
    let added=client.call_tool_ok("orbit_auto_task_add",json!({"workspace":selector,"name":"stdio-chore","description":"Initial","schedule":{"every_minutes":60},"template":{"title":"Wire chore","description":"No provider dispatch","acceptance_criteria":["Persisted"],"complexity":"low","status":"proposed"}}));
    assert_eq!(added["name"], "stdio-chore");
    let definition = workspace.work.join(".orbit/auto_tasks/stdio-chore.yaml");
    client.call_tool_ok(
        "orbit_auto_task_update",
        json!({"workspace":selector,"name":"stdio-chore","description":"Updated over stdio"}),
    );
    client.call_tool_ok(
        "orbit_auto_task_update",
        json!({"workspace":selector,"name":"stdio-chore","enabled":false,"expected_enabled":true}),
    );
    // The compare is atomic: a stale expectation changes nothing.
    client.call_tool_err(
        "orbit_auto_task_update",
        json!({"workspace":selector,"name":"stdio-chore","enabled":true,"expected_enabled":true}),
    );
    let bytes = std::fs::read(&definition).unwrap();
    drop(client);
    let mut client = workspace.serve_with_args(&["--operator"]);
    let definitions = client.call_tool_ok("orbit_auto_task_list", json!({"workspace":selector}));
    let chore = definitions["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == "stdio-chore")
        .unwrap();
    assert_eq!(chore["description"], "Updated over stdio");
    assert_eq!(chore["enabled"], false);
    client.call_tool_err(
        "orbit_auto_task_update",
        json!({"workspace":"ws_missing","name":"stdio-chore","description":"Wrong workspace"}),
    );
    assert_eq!(std::fs::read(&definition).unwrap(), bytes);
    let minted = client.call_tool_ok(
        "orbit_auto_task_mint",
        json!({"workspace":selector,"name":"stdio-chore"}),
    );
    assert_eq!(minted["status"], "proposed");
    let task_id = minted["id"].as_str().unwrap().to_owned();
    // Deletion is a CLI-only operation: MCP does not offer it.
    client.call_tool_err(
        "orbit_auto_task_delete",
        json!({"workspace":selector,"name":"stdio-chore","force":true}),
    );
    drop(client);
    let refused = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args(["auto-task", "delete", "stdio-chore"])
        .output()
        .unwrap();
    assert!(!refused.status.success(), "{refused:?}");
    assert_eq!(
        std::fs::read(&definition).unwrap(),
        bytes,
        "open-mint deletion refusal preserves definition"
    );
    orbit_ok(
        McpWorkspace::orbit_command(&workspace.work, &workspace.home).args([
            "auto-task",
            "delete",
            "stdio-chore",
            "--force",
        ]),
    );
    assert!(!definition.exists());
    let mut client = workspace.serve_with_args(&["--operator"]);
    let definitions = client.call_tool_ok("orbit_auto_task_list", json!({"workspace":selector}));
    assert!(
        !definitions["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["name"] == "stdio-chore")
    );
    let task = client.call_tool_ok(
        "orbit_task_show",
        json!({"workspace":selector,"id":task_id}),
    );
    assert_eq!(
        task["status"], "proposed",
        "forced definition deletion keeps the minted task"
    );
    let runs = client.call_tool_ok("orbit_workflow_run_list", json!({"workspace":selector}));
    assert!(
        runs["items"].as_array().unwrap().is_empty(),
        "CRUD and mint dispatch no jobs"
    );
}

fn seeded_run(
    workspace: &McpWorkspace,
    id: &str,
    job: &str,
    state: &str,
    input: Value,
) -> orbit_core::OrbitRuntime {
    let runtime = orbit_core::OrbitRuntime::from_roots(
        &workspace.home.join(".orbit"),
        &workspace.work.join(".orbit"),
    )
    .unwrap();
    Connection::open(workspace.home.join(".orbit/orbit.db")).unwrap().execute(
        "INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,input_json,scheduled_at,started_at,finished_at,created_at,pid) VALUES (?1,?2,?3,1,?4,?5,?6,?6,?7,?6,?8)",
        rusqlite::params![id,runtime.workspace_id().unwrap(),job,state,input.to_string(),chrono::Utc::now().to_rfc3339(),if state=="running"{None}else{Some(chrono::Utc::now().to_rfc3339())},if state=="running"{Some(std::process::id())}else{None}],
    ).unwrap();
    runtime
        .write_run_state(
            id,
            &orbit_types::workflow::PipelineState::new(id.into(), job.into(), input),
        )
        .unwrap();
    runtime
}

#[test]
fn stdio_workers_changes_one_running_record_and_preserves_it_on_stale_or_unauthorized_calls() {
    let workspace = McpWorkspace::init();
    let selector = workspace.work.to_str().unwrap();
    let id = "jrun-stdio-workers";
    let runtime = seeded_run(
        &workspace,
        id,
        "workspace_auto_pipeline",
        "running",
        json!({"max_active_leaf_runs":2}),
    );
    let original = runtime.show_job_run(id).unwrap();
    let mut client = workspace.serve_with_args(&["--operator"]);
    let changed=client.call_tool_ok("orbit_workflow_auto",json!({"workspace":selector,"action":"resize","id":id,"concurrency":3,"if_revision":0,"reason":"Wire fixture"}));
    assert_eq!(changed["outcome"], "updated");
    assert_eq!(changed["revision"], 1);
    drop(client);
    let mut client = workspace.serve_with_args(&["--operator"]);
    let unchanged = client.call_tool_ok(
        "orbit_workflow_auto",
        json!({"workspace":selector,"action":"resize","concurrency":3,"if_revision":1}),
    );
    assert_eq!(unchanged["outcome"], "unchanged");
    assert_eq!(unchanged["revision"], 1);
    let before = runtime.read_run_state(id).unwrap().unwrap();
    client.call_tool_err(
        "orbit_workflow_auto",
        json!({"workspace":selector,"action":"resize","id":id,"concurrency":4,"if_revision":0}),
    );
    client.call_tool_err(
        "orbit_workflow_auto",
        json!({"workspace":"ws_missing","action":"resize","id":id,"concurrency":4}),
    );
    assert_eq!(runtime.read_run_state(id).unwrap().unwrap(), before);
    let current = runtime.show_job_run(id).unwrap();
    assert_eq!(current.run_id, original.run_id);
    assert_eq!(current.state, original.state);
    assert_eq!(current.input, original.input);
    drop(client);
    let mut client = workspace.serve();
    assert_eq!(
        client.call_tool_err(
            "orbit_workflow_auto",
            json!({"workspace":selector,"action":"resize","id":id,"concurrency":4})
        )["code"],
        "capability_denied"
    );
    assert_eq!(runtime.read_run_state(id).unwrap().unwrap(), before);
}

/// A start window longer than the drain itself accepts is refused before any
/// run exists, and resize retunes a replica's pull drain — the workspace's one
/// live drain when no `id` is given — past the former leaf ceiling of ten.
#[test]
fn stdio_drain_window_matches_the_drain_and_resize_reaches_a_pull_drain() {
    let workspace = McpWorkspace::init();
    let selector = workspace.work.to_str().unwrap();
    let id = "jrun-stdio-pull-workers";
    let runtime = seeded_run(
        &workspace,
        id,
        "workspace_pull_pipeline",
        "running",
        json!({"max_active_leaf_runs":5}),
    );
    let mut client = workspace.serve_with_args(&["--operator"]);
    let refused = client.call_tool_err(
        "orbit_workflow_auto",
        json!({"workspace":selector,"action":"start","for_seconds":86_401}),
    );
    assert_eq!(refused["code"], "invalid_input", "{refused}");
    let runs = client.call_tool_ok("orbit_workflow_run_list", json!({"workspace":selector}));
    assert_eq!(
        runs["items"].as_array().unwrap().len(),
        1,
        "only the seeded pull drain exists: {runs}"
    );

    let changed = client.call_tool_ok(
        "orbit_workflow_auto",
        json!({"workspace":selector,"action":"resize","concurrency":24}),
    );

    assert_eq!(changed["run_id"], id);
    assert_eq!(changed["previous_concurrency"], 5);
    assert_eq!(changed["concurrency"], 24);
    let state = runtime.read_run_state(id).unwrap().unwrap();
    assert_eq!(state.effective_max_active_leaf_runs(5), 24);
}

#[test]
fn stdio_resume_runs_only_deterministic_remaining_steps_and_reopens_checkpoint_lineage() {
    let workspace = McpWorkspace::init();
    let selector = workspace.work.to_str().unwrap();
    let jobs = workspace.home.join(".orbit/resources/jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    std::fs::write(jobs.join("stdio-resume.yaml"),"schemaVersion: 2\nkind: Job\nmetadata:\n  name: stdio-resume\nspec:\n  state: enabled\n  kind: workflow\n  steps:\n    - id: first\n      spec:\n        type: deterministic\n        action: sleep\n        config: {}\n    - id: second\n      spec:\n        type: deterministic\n        action: sleep\n        config: {}\n").unwrap();
    let source = "jrun-stdio-resume-source";
    let runtime = seeded_run(
        &workspace,
        source,
        "stdio-resume",
        "failed",
        json!({"seconds":0}),
    );
    let mut checkpoint = runtime.read_run_state(source).unwrap().unwrap();
    checkpoint.record_step(
        0,
        orbit_types::workflow::JobRunState::Success,
        Some(json!({"sentinel":"preserve successful checkpoint"})),
        None,
    );
    checkpoint.record_pipeline_output(
        "first",
        json!({"sentinel":"preserve successful checkpoint"}),
    );
    runtime.write_run_state(source, &checkpoint).unwrap();
    let mut client = workspace.serve_with_args(&["--operator"]);
    let submitted = client.call_tool_ok(
        "orbit_workflow_run_resume",
        json!({"workspace":selector,"id":source,"model":"codex"}),
    );
    let resumed = submitted["run_id"].as_str().unwrap().to_owned();
    assert_ne!(resumed, source);
    assert_eq!(submitted["retry_source_run_id"], source);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let run = client.call_tool_ok(
            "orbit_workflow_run_show",
            json!({"workspace":selector,"id":resumed}),
        );
        if run["state"] == "success" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "deterministic resumed worker did not settle: {run}"
        );
        assert!(
            run["state"] != "failed",
            "deterministic resumed worker failed: {run}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(client);
    let mut client = workspace.serve_with_args(&["--operator"]);
    let reopened = client.call_tool_ok(
        "orbit_workflow_run_show",
        json!({"workspace":selector,"id":resumed}),
    );
    assert_eq!(reopened["state"], "success");
    assert_eq!(reopened["retry_source_run_id"], source);
    let state = runtime.read_run_state(&resumed).unwrap().unwrap();
    assert_eq!(
        state.pipeline["first"]["sentinel"],
        "preserve successful checkpoint"
    );
    assert_eq!(
        runtime.show_job_run(source).unwrap().state,
        orbit_types::workflow::JobRunState::Failed
    );
    assert_eq!(
        runtime.read_run_state(source).unwrap().unwrap(),
        checkpoint,
        "resume preserves its source checkpoints"
    );
    client.call_tool_err(
        "orbit_workflow_run_resume",
        json!({"workspace":selector,"id":resumed,"model":"codex"}),
    );
    drop(client);
    let mut client = workspace.serve();
    assert_eq!(
        client.call_tool_err(
            "orbit_workflow_run_resume",
            json!({"workspace":selector,"id":source})
        )["code"],
        "capability_denied"
    );
}

#[test]
fn review_reset_requires_an_operator_and_audits_cli_and_mcp_decisions() {
    const CHILD: &str = "ORBIT_REVIEW_RESET_BOUNDARY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let home = tempdir().unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        test_env::clear_inherited_authority(|key| {
            command.env_remove(key);
        });
        let name = "mcp_roundtrip::transport_operations::review_reset_requires_an_operator_and_audits_cli_and_mcp_decisions";
        let output = command
            .args(["--exact", name, "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));
        return;
    }
    let workspace = McpWorkspace::init();
    let selector = workspace.work.to_str().unwrap();
    let mut client = workspace.serve();
    let task = client.call_tool_ok("orbit_task_add", json!({"title":"Review reset proof","description":"Disposable review lineage","complexity":"low","model":"codex"}));
    let id = task["id"].as_str().unwrap();
    drop(client);
    let runtime = orbit_core::OrbitRuntime::from_roots(
        &workspace.home.join(".orbit"),
        &workspace.work.join(".orbit"),
    )
    .unwrap();
    let ws = runtime.workspace_id().unwrap();
    // Seed the persisted shape produced before timeout accounting was fixed,
    // deliberately omitting `decisions` and carrying the retired start and
    // repair-cycle limits to exercise compatibility.
    let lineage = format!("{ws}/{id}/main");
    let now = chrono::Utc::now();
    let ledger = json!({
        "lineage_key": lineage, "task_ids": [id], "budget": {"reviewer_starts": 1, "repair_cycles": 2, "minutes": 120},
        "attempts": [{"attempt_id": "rvw-fixture-1", "index": 1, "run_id": "old-run", "task_meaning_digest": "old", "candidate": {"commit": "candidate", "tree": "tree"}, "started_at": now,
        "state": {"state": "settled", "verdict": "incomplete"}, "repair_cycles": 0, "elapsed_seconds": 19385}],
        "consumed_seconds": 19385, "revision": 1, "updated_at": now,
    });
    // How that ledger reads today: the retired limits drop out.
    let mut current = ledger.clone();
    for retired in ["reviewer_starts", "repair_cycles"] {
        current["budget"].as_object_mut().unwrap().remove(retired);
    }
    current["attempts"][0]
        .as_object_mut()
        .unwrap()
        .remove("repair_cycles");
    let conn = Connection::open(workspace.home.join(".orbit/orbit.db")).unwrap();
    conn.execute("INSERT INTO review_lineages(workspace_id,lineage_key,revision,ledger_json) VALUES(?1,?2,1,?3)", rusqlite::params![ws,lineage,ledger.to_string()]).unwrap();
    let persisted = || {
        conn.query_row(
            "SELECT ledger_json FROM review_lineages WHERE lineage_key=?1",
            [&lineage],
            |row| row.get::<_, String>(0),
        )
        .unwrap()
    };
    let original = persisted();
    let args = json!({"workspace":selector,"id":id,"lineage_key":lineage,"reason":"Repair old timeout charges"});
    let mut client = workspace.serve();
    client.call_tool_err("orbit_task_review_reset", args.clone());
    assert_eq!(
        persisted(),
        original,
        "ordinary MCP authority cannot reset an exhausted ledger"
    );
    drop(client);
    let denied = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args([
            "task",
            "review-reset",
            id,
            "--lineage",
            &lineage,
            "--reason",
            "agent override",
        ])
        .env("ORBIT_AGENT_NAME", "fixture-agent")
        .output()
        .unwrap();
    assert!(!denied.status.success());
    assert_eq!(persisted(), original);
    let elevated_agent = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args([
            "task",
            "review-reset",
            id,
            "--lineage",
            &lineage,
            "--reason",
            "managed override",
        ])
        .env("ORBIT_AGENT_NAME", "fixture-agent")
        .env("ORBIT_OPERATOR", "1")
        .output()
        .unwrap();
    assert!(
        !elevated_agent.status.success(),
        "a managed agent cannot use elevated operator authority to reset its budget"
    );
    assert_eq!(persisted(), original);
    let mut client = workspace.serve_with_args(&["--operator"]);
    client.call_tool_err(
        "orbit_task_review_reset",
        json!({"workspace":selector,"id":id,"lineage_key":lineage,"reason":" "}),
    );
    assert_eq!(persisted(), original, "a blank decision changes nothing");
    let reset = client.call_tool_ok("orbit_task_review_reset", args);
    assert_eq!(reset["ledger"]["attempts"], current["attempts"]);
    assert_eq!(reset["ledger"]["consumed_seconds"], 0);
    assert_eq!(
        reset["ledger"]["decisions"][0]["previous_consumption"]["seconds"],
        19385
    );
    assert_eq!(
        reset["ledger"]["decisions"][0]["reason"],
        "Repair old timeout charges"
    );
    assert!(
        !reset["ledger"]["decisions"][0]["actor"]
            .as_str()
            .unwrap()
            .is_empty()
    );
    assert_eq!(reset["ledger"]["budget"], current["budget"]);
    drop(client);
    let output = orbit_ok(
        McpWorkspace::orbit_command(&workspace.work, &workspace.home)
            .args([
                "task",
                "review-reset",
                id,
                "--lineage",
                &lineage,
                "--reason",
                "Explicit current budget",
                "--adopt-configured-budget",
                "--json",
            ])
            .env("ORBIT_OPERATOR", "1"),
    );
    let reset: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(reset["ledger"]["attempts"], current["attempts"]);
    assert_eq!(reset["ledger"]["decisions"].as_array().unwrap().len(), 2);
    assert_eq!(
        reset["ledger"]["decisions"][1]["reason"],
        "Explicit current budget"
    );
    assert_eq!(
        reset["ledger"]["budget"],
        serde_json::to_value(runtime.operation_policy().review_budget()).unwrap()
    );
    assert_eq!(
        serde_json::from_str::<Value>(&persisted()).unwrap(),
        reset["ledger"],
        "the CLI decision survives reopening the store"
    );
}

/// An operator who starts a drain through MCP with no
/// `workflow.required_validation_commands` gets a note, not a refusal: an
/// empty list means no required check, and the drain runs.
#[test]
fn stdio_drain_start_notes_that_no_required_validation_runs() {
    let workspace = McpWorkspace::init();
    let selector = workspace.work.to_str().unwrap();
    let jobs = workspace.home.join(".orbit/resources/jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    std::fs::write(
        jobs.join("workspace_auto_pipeline.yaml"),
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: workspace_auto_pipeline\nspec:\n  state: enabled\n  kind: workflow\n  steps:\n    - id: nap\n      default_input:\n        seconds: 0\n      spec:\n        type: deterministic\n        action: sleep\n        config: {}\n",
    )
    .unwrap();
    let mut client = workspace.serve_with_args(&["--operator"]);

    let started = client.call_tool_ok(
        "orbit_workflow_auto",
        json!({"workspace":selector,"action":"start","for_seconds":60}),
    );

    assert!(
        started["note"]
            .as_str()
            .is_some_and(|note| note.contains("workflow.required_validation_commands")),
        "the start names the empty key it read: {started}"
    );
    let run_id = started["run_id"].as_str().expect("the drain starts");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let run = client.call_tool_ok(
            "orbit_workflow_run_show",
            json!({"workspace":selector,"id":run_id}),
        );
        if run["state"] == "success" {
            break;
        }
        assert!(
            Instant::now() < deadline && run["state"] != "failed",
            "the drain runs to completion: {run}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// [ORB-13987] With login-shell resolution disabled and no configured PATH,
/// required validation runs with whatever PATH launched the worker. The MCP
/// drain status and the CLI doctor both say so before a drain is started;
/// without required commands there is nothing to warn about. Doctor also
/// reports configured tool shadowing, the selected startup mode, and the
/// reason a broken interactive rc falls back to login-only resolution.
#[test]
fn drain_status_and_doctor_warn_when_validation_cannot_use_the_login_shell() {
    let workspace = McpWorkspace::init();
    let selector = workspace.work.to_str().unwrap();
    let configure = |key: &str, value: &str| {
        orbit_ok(
            McpWorkspace::orbit_command(&workspace.work, &workspace.home).args([
                "config",
                "set",
                "--seed-from-global",
                key,
                value,
            ]),
        );
    };
    let observe = || {
        let mut client = workspace.serve_with_args(&["--operator"]);
        let readiness = client.call_tool_ok(
            "orbit_workflow_auto",
            json!({"workspace":selector,"action":"status"}),
        );
        let doctor = orbit_ok(
            McpWorkspace::orbit_command(&workspace.work, &workspace.home)
                .args(["doctor", "--json"]),
        );
        let rows: Value = serde_json::from_slice(&doctor.stdout).unwrap();
        let row = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["check"] == "validation-env")
            .cloned()
            .unwrap_or_else(|| panic!("doctor has a validation-env row: {rows}"));
        (readiness["validation_env_warning"].clone(), row)
    };

    configure("workflow.validation_env.login_shell", "false");
    let (warning, row) = observe();
    assert_eq!(
        warning,
        Value::Null,
        "no required commands, nothing to warn"
    );
    assert_eq!(row["status"], "skipped", "{row}");

    configure(
        "workflow.required_validation_commands",
        r#"["make ci-fast"]"#,
    );
    let (warning, row) = observe();
    let warning = warning.as_str().expect("the drain status warns");
    assert!(
        warning.contains("`workflow.validation_env.login_shell = false`")
            && warning.contains("source: launcher_fallback")
            && warning.contains("PATH="),
        "the warning names the disabled resolution, the fallback and its PATH: {warning}"
    );
    assert_eq!(row["status"], "warning", "{row}");
    assert!(
        row["message"].as_str().unwrap().contains(warning),
        "doctor includes the same warning: {row}"
    );
    assert!(
        row["remediation"]
            .as_str()
            .is_some_and(|fix| fix.contains("workflow.validation_env.path")),
        "{row}"
    );

    // The validation PATH is separate from the fixture command's PATH, so
    // these stubs affect only doctor resolution, never workspace setup.
    let first = workspace.home.join("validation-first");
    let later = workspace.home.join("validation-later");
    for tool in ["python3", "git", "make"] {
        plant_agent_cli_stub(&first, tool);
        plant_agent_cli_stub(&later, tool);
    }
    configure("workflow.validation_env.path_mode", "replace");
    configure(
        "workflow.validation_env.path",
        &json!([first, later]).to_string(),
    );
    let (_, row) = observe();
    assert_eq!(row["status"], "warning", "shadowing is advisory: {row}");
    let message = row["message"].as_str().unwrap();
    assert!(message.contains("probe mode: disabled"), "{message}");
    assert!(
        message.contains(&format!("PATH={}:{}", first.display(), later.display())),
        "{message}"
    );
    for tool in ["python3", "git", "make"] {
        assert!(
            message.contains(&format!(
                "{tool} resolves to {}",
                first.join(tool).display()
            )),
            "{message}"
        );
        assert!(
            message.contains(&format!("{tool}: {} shadows", first.join(tool).display())),
            "{message}"
        );
        assert!(
            message.contains(&later.join(tool).display().to_string()),
            "{message}"
        );
    }

    configure("workflow.validation_env.login_shell", "true");
    configure("workflow.validation_env.interactive", "false");
    let (_, row) = observe();
    assert!(
        row["message"]
            .as_str()
            .unwrap()
            .contains("probe mode: login;"),
        "the setting reaches the runtime resolver: {row}"
    );
    configure("workflow.validation_env.interactive", "true");
    let (_, row) = observe();
    assert!(
        row["message"]
            .as_str()
            .unwrap()
            .contains("probe mode: interactive_login;"),
        "{row}"
    );

    // Fixture rc files affect only the probe, with no dependency on the
    // operator's dotfiles. Cover the common account shells on Linux/macOS.
    for profile in [".bash_profile", ".profile"] {
        std::fs::write(workspace.home.join(profile), ". \"$HOME/.bashrc\"\n").unwrap();
    }
    for rc in [".bashrc", ".zshrc"] {
        std::fs::write(workspace.home.join(rc), "case $- in *i*) exit 42 ;; esac\n").unwrap();
    }
    let fish = workspace.home.join(".config/fish");
    std::fs::create_dir_all(&fish).unwrap();
    std::fs::write(
        fish.join("config.fish"),
        "if status is-interactive\nexit 42\nend\n",
    )
    .unwrap();
    let (_, row) = observe();
    assert_eq!(row["status"], "warning", "fallback is advisory: {row}");
    let message = row["message"].as_str().unwrap();
    assert!(
        message.contains("probe mode: login;")
            && message.contains("interactive fallback reason:")
            && message.contains("exited with status 42"),
        "{message}"
    );
}
