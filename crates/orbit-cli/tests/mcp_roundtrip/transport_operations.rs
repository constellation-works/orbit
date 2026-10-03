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
