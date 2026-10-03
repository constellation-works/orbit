//! Exercise desktop contracts through the registered production tool boundary.
use super::super::test_support::{run_tool_as_operator, test_runtime, unmanaged_tool_env_guard};
use crate::adapter::command::ToolEntryPoint;
use orbit_common::OrbitError;
use orbit_types::tool::ToolSessionContext;
use serde_json::json;

#[test]
fn desktop_tools_require_explicit_destination_and_reject_undeclared_authority() {
    if !isolated("desktop_tools_require_explicit_destination_and_reject_undeclared_authority") {
        return;
    }
    let _guard = unmanaged_tool_env_guard();
    let (_root, runtime, repo) = test_runtime();
    assert!(
        runtime
            .run_tool("orbit.task.list", json!({"view":"bounded"}))
            .is_err()
    );
    let result = runtime.run_tool("orbit.task.add", json!({"workspace":repo,"request_id":"spoof","title":"No spoof","description":"","acceptance_criteria":["Proof"],"actor":"human"}));
    assert!(result.is_err(), "actor cannot become a trusted grant");
}

#[test]
fn desktop_run_reads_preserve_operator_gate_at_mcp_entrypoint() {
    if !isolated("desktop_run_reads_preserve_operator_gate_at_mcp_entrypoint") {
        return;
    }
    let _guard = unmanaged_tool_env_guard();
    let (_root, runtime, repo) = test_runtime();
    let denied = runtime.execute_tool_command_dispatch_with_session_context(
        "orbit.workflow.run.list",
        json!({"workspace":repo,"view":"bounded"}),
        None,
        None,
        ToolEntryPoint::Mcp,
        ToolSessionContext::default(),
    );
    assert!(
        matches!(denied, Err(OrbitError::CapabilityDenied(_))),
        "desktop is not a backdoor to operator run evidence: {denied:?}"
    );
    let read = run_tool_as_operator(
        &runtime,
        "orbit.workflow.run.list",
        json!({"workspace":repo,"view":"bounded"}),
    )
    .expect("operator observed read");
    assert_eq!(read["items"], json!([]));
    assert_eq!(read["total"], 0);
}

#[test]
fn desktop_create_and_comment_retry_through_tool_boundary_have_one_effect() {
    if !isolated("desktop_create_and_comment_retry_through_tool_boundary_have_one_effect") {
        return;
    }
    let _guard = unmanaged_tool_env_guard();
    let (_root, runtime, repo) = test_runtime();
    let request = json!({"workspace":repo,"model":"codex","request_id":"create-1","title":"Capture","description":"Daily work","acceptance_criteria":["Observed proof"],"priority":"medium"});
    let created = runtime
        .run_tool("orbit.task.add", request.clone())
        .expect("create proposed task");
    let retried = runtime
        .run_tool("orbit.task.add", request.clone())
        .expect("reconcile create reply");
    assert_eq!(
        created["snapshot"]["task"]["id"],
        retried["snapshot"]["task"]["id"]
    );
    assert_eq!(created["snapshot"]["task"]["status"], "proposed");
    let mut changed = request;
    changed["title"] = json!("Changed request");
    let refused = runtime
        .run_tool("orbit.task.add", changed)
        .expect("definite precommit refusal");
    assert_eq!(refused["mutation_applied"], false);
    assert!(refused["refusal"]["message"].is_string());
    let id = created["snapshot"]["task"]["id"].clone();
    let comment = json!({"workspace":repo,"model":"codex","request_id":"comment-1","id":id,"expected_revision":retried["snapshot"]["revision"],"comment":"One durable comment"});
    let first = runtime
        .run_tool("orbit.task.update", comment.clone())
        .expect("comment");
    let second = runtime
        .run_tool("orbit.task.update", comment)
        .expect("same comment retry");
    assert_eq!(
        first["snapshot"]["comments_total"],
        second["snapshot"]["comments_total"]
    );
    assert_eq!(second["replayed"], true);
    let conflict = runtime.run_tool("orbit.task.update", json!({"workspace":repo,"request_id":"stale-1","id":id,"expected_revision":created["snapshot"]["revision"],"title":"stale"})).expect("structured stale response");
    assert_eq!(conflict["conflict"]["code"], "revision_conflict");
    assert_eq!(conflict["snapshot"]["task"]["title"], "Capture");
    assert_eq!(
        conflict["snapshot"]["revision"],
        second["snapshot"]["revision"]
    );
}

/// A live auto drain with checkpointed state — the shape a resize addresses.
fn live_auto_drain(runtime: &crate::OrbitRuntime) -> String {
    let jobs = runtime.stores().jobs();
    let run = jobs
        .insert_job_run(
            "workspace_auto_pipeline",
            1,
            chrono::Utc::now(),
            Some(json!({"max_active_leaf_runs": 2})),
            None,
        )
        .expect("insert drain run");
    jobs.mark_job_run_running(&run.run_id, chrono::Utc::now(), std::process::id())
        .expect("start drain run");
    let state = orbit_types::workflow::PipelineState::new(
        run.run_id.clone(),
        "workspace_auto_pipeline".to_string(),
        json!({"max_active_leaf_runs": 2}),
    );
    jobs.write_run_state(&run.run_id, &state)
        .expect("write drain state");
    run.run_id
}

/// `resize` retunes the one live drain in place; it refuses to guess when
/// there is none or several, and its inputs are refused on other actions.
#[test]
fn desktop_drain_resize_targets_the_one_live_drain_without_replacing_it() {
    if !isolated("desktop_drain_resize_targets_the_one_live_drain_without_replacing_it") {
        return;
    }
    let _guard = unmanaged_tool_env_guard();
    let (_root, runtime, repo) = test_runtime();
    let resize = |fields: serde_json::Value| {
        let mut input = fields;
        input["workspace"] = json!(repo);
        input["action"] = json!("resize");
        run_tool_as_operator(&runtime, "orbit.workflow.auto", input)
    };

    let none_live = resize(json!({"concurrency": 1}));
    assert!(
        matches!(none_live, Err(OrbitError::InvalidInput(_))),
        "{none_live:?}"
    );
    let misplaced = run_tool_as_operator(
        &runtime,
        "orbit.workflow.auto",
        json!({"workspace":repo,"action":"stop","if_revision":0}),
    );
    assert!(
        matches!(misplaced, Err(OrbitError::InvalidInput(_))),
        "{misplaced:?}"
    );

    let drain = live_auto_drain(&runtime);
    let missing = resize(json!({}));
    assert!(
        matches!(missing, Err(OrbitError::InvalidInput(_))),
        "{missing:?}"
    );
    let resized = resize(json!({"concurrency": 1, "reason": "Free a worker"})).expect("resize");
    assert_eq!(resized["action"], "resize");
    assert_eq!(resized["run_id"], json!(drain));
    assert_eq!(resized["outcome"], "updated");
    assert_eq!(resized["previous_concurrency"], 2);
    assert_eq!(resized["concurrency"], 1);
    let run = runtime.show_job_run(&drain).expect("drain run");
    assert_eq!(
        run.state,
        orbit_types::workflow::JobRunState::Running,
        "a resize keeps the run it retunes"
    );

    let second = live_auto_drain(&runtime);
    let ambiguous = resize(json!({"concurrency": 2}));
    assert!(
        matches!(ambiguous, Err(OrbitError::InvalidInput(_))),
        "{ambiguous:?}"
    );
    let named = resize(json!({"id": second, "concurrency": 1})).expect("resize a named drain");
    assert_eq!(named["run_id"], json!(second));
}

fn isolated(name: &str) -> bool {
    const MARKER: &str = "ORBIT_TEST_DESKTOP_TOOL_CHILD";
    let module = module_path!()
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap();
    let exact = format!("{module}::{name}");
    if std::env::var_os(MARKER).is_some_and(|value| value == exact.as_str()) {
        return true;
    }
    let home = tempfile::tempdir().expect("fixture home");
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let output = command
        .args(["--exact", &exact, "--nocapture", "--test-threads=1"])
        .env_remove("ORBIT_WORKER_CONTEXT_REQUIRED")
        .env(MARKER, &exact)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path())
        .output()
        .expect("isolated fixture child");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed;"));
    false
}

#[test]
fn desktop_drain_requires_operator_and_validates_before_dispatch() {
    if !isolated("desktop_drain_requires_operator_and_validates_before_dispatch") {
        return;
    }
    let _guard = unmanaged_tool_env_guard();
    let (_root, runtime, repo) = test_runtime();
    for input in [
        json!({"workspace":repo,"action":"start","for_seconds":3600,"complete":true}),
        json!({"workspace":repo,"action":"stop"}),
    ] {
        let denied = runtime.execute_tool_command_dispatch_with_session_context(
            "orbit.workflow.auto",
            input,
            None,
            None,
            ToolEntryPoint::Mcp,
            ToolSessionContext::default(),
        );
        assert!(
            matches!(denied, Err(OrbitError::CapabilityDenied(_))),
            "UI cannot grant drain authority: {denied:?}"
        );
    }
    let denied = runtime.execute_tool_command_dispatch_with_session_context(
        "orbit.workflow.auto",
        json!({"workspace":repo,"action":"status"}),
        None,
        None,
        ToolEntryPoint::Mcp,
        ToolSessionContext::default(),
    );
    assert!(matches!(denied, Err(OrbitError::CapabilityDenied(_))));
    for fields in [
        json!({"for_seconds":0}),
        json!({"for_seconds":604801}),
        json!({"for_seconds":3600,"concurrency":0}),
        json!({"for_seconds":3600,"concurrency":4294967296u64}),
        json!({"for_seconds":3600,"complete":"true"}),
    ] {
        let mut input = fields;
        input["workspace"] = json!(repo);
        input["action"] = json!("start");
        assert!(run_tool_as_operator(&runtime, "orbit.workflow.auto", input).is_err());
    }
    assert!(
        run_tool_as_operator(
            &runtime,
            "orbit.workflow.auto",
            json!({"workspace":repo,"action":"stop","complete":true})
        )
        .is_err()
    );
    let runs = run_tool_as_operator(
        &runtime,
        "orbit.workflow.run.list",
        json!({"workspace":repo,"view":"bounded"}),
    )
    .expect("runs");
    assert_eq!(runs["total"], 0, "refused starts created no run");
}

#[test]
fn desktop_drain_readiness_and_idle_stop_reuse_runtime_without_dispatch() {
    if !isolated("desktop_drain_readiness_and_idle_stop_reuse_runtime_without_dispatch") {
        return;
    }
    let _guard = unmanaged_tool_env_guard();
    let (_root, runtime, repo) = test_runtime();
    let read = run_tool_as_operator(
        &runtime,
        "orbit.workflow.auto",
        json!({"workspace":repo,"action":"status"}),
    )
    .expect("readiness");
    assert_eq!(read["schema_version"], 1);
    assert_eq!(read["snapshot"]["read_only"], true);
    assert!(read["capacity"].is_object());
    assert_eq!(read["controls_authorized"], true);
    let stopped = run_tool_as_operator(
        &runtime,
        "orbit.workflow.auto",
        json!({"workspace":repo,"action":"stop"}),
    )
    .expect("idle stop");
    assert_eq!(stopped["outcome"], "idle");
    assert_eq!(stopped["coordinators"], json!([]));
    let runs = run_tool_as_operator(
        &runtime,
        "orbit.workflow.run.list",
        json!({"workspace":repo,"view":"bounded"}),
    )
    .expect("runs");
    assert_eq!(runs["total"], 0);
}

#[test]
fn domain_automation_scopes_definitions_checks_conflicts_and_mints_without_dispatch() {
    if !isolated("domain_automation_scopes_definitions_checks_conflicts_and_mints_without_dispatch")
    {
        return;
    }
    let _guard = unmanaged_tool_env_guard();
    let (_root, runtime, repo) = test_runtime();
    runtime.run_tool("orbit.auto_task.add",json!({"workspace":repo,"name":"desktop-chore","description":"An observable chore","schedule":{"every_minutes":60},"template":{"title":"Desktop chore","description":"Check the workspace","acceptance_criteria":["Evidence recorded"],"task_type":"chore","status":"proposed"}})).expect("seed auto-task");
    let jobs = runtime.paths().global_dir.join("resources/jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    std::fs::write(jobs.join("desktop-maintenance.yaml"),"schemaVersion: 2\nkind: Job\nmetadata:\n  name: desktop-maintenance\nspec:\n  state: enabled\n  kind: workflow\n  max_active_runs: 1\n  steps:\n    - id: nap\n      spec:\n        type: deterministic\n        action: sleep\n        config: {}\n").unwrap();
    let routines = runtime.shared_root().join("routines");
    std::fs::create_dir_all(&routines).unwrap();
    std::fs::write(routines.join("desktop-routine.yaml"),"schemaVersion: 1\nname: desktop-routine\nenabled: true\ntrigger:\n  cron: '0 9 * * *'\ntarget: job:desktop-maintenance\n").unwrap();
    fn read(
        runtime: &crate::OrbitRuntime,
        repo: &std::path::Path,
        scope: &str,
        limit: usize,
    ) -> serde_json::Value {
        let (name, input) = match scope {
            "routines" => (
                "orbit.routine.control",
                json!({"workspace":repo,"action":"list","limit":limit}),
            ),
            "auto_tasks" => (
                "orbit.auto_task.list",
                json!({"workspace":repo,"view":"bounded","limit":limit}),
            ),
            "jobs" => (
                "orbit.workflow.run.list",
                json!({"workspace":repo,"view":"bounded","include_catalog":true,"limit":limit}),
            ),
            _ => unreachable!(),
        };
        let value = run_tool_as_operator(runtime, name, input).expect("read definitions");
        if scope == "jobs" {
            value["catalog"].clone()
        } else {
            value
        }
    }
    // Anonymous sessions cannot inspect operator run/schedule state or mutate it.
    for (scope, name, input) in [
        (
            "routines",
            "orbit.routine.control",
            json!({"workspace":repo,"action":"list"}),
        ),
        (
            "auto_tasks",
            "orbit.auto_task.list",
            json!({"workspace":repo,"view":"bounded"}),
        ),
        (
            "jobs",
            "orbit.workflow.run.list",
            json!({"workspace":repo,"view":"bounded","include_catalog":true}),
        ),
    ] {
        let denied = runtime.execute_tool_command_dispatch_with_session_context(
            name,
            input,
            None,
            None,
            ToolEntryPoint::Mcp,
            ToolSessionContext::default(),
        );
        assert!(matches!(denied, Err(OrbitError::CapabilityDenied(_))));
        let data = read(&runtime, &repo, scope, 1);
        assert!(data["items"].as_array().unwrap().len() <= 1);
    }
    let read = read(&runtime, &repo, "routines", 25);
    assert_eq!(read["items"][0]["name"], "desktop-routine", "{read}");
    let mut toggle = json!({"workspace":repo,"action":"toggle","name":"desktop-routine","target":"job:desktop-maintenance","expected_enabled":true,"enabled":false});
    let denied = runtime.execute_tool_command_dispatch_with_session_context(
        "orbit.routine.control",
        toggle.clone(),
        None,
        None,
        ToolEntryPoint::Mcp,
        ToolSessionContext::default(),
    );
    assert!(matches!(denied, Err(OrbitError::CapabilityDenied(_))));
    toggle["target"] = json!("job:changed-target");
    assert!(run_tool_as_operator(&runtime, "orbit.routine.control", toggle.clone()).is_err());
    toggle["target"] = json!("job:desktop-maintenance");
    assert_eq!(
        run_tool_as_operator(&runtime, "orbit.routine.control", toggle.clone()).unwrap()["enabled"],
        false
    );
    assert!(
        run_tool_as_operator(&runtime, "orbit.routine.control", toggle).is_err(),
        "stale toggle refused"
    );
    let toggle =
        json!({"workspace":repo,"name":"desktop-chore","expected_enabled":true,"enabled":false});
    let mut mixed = toggle.clone();
    mixed["description"] = json!("edited alongside the toggle");
    assert!(
        run_tool_as_operator(&runtime, "orbit.auto_task.update", mixed).is_err(),
        "a checked toggle carries no other edit"
    );
    assert!(
        runtime
            .auto_task_show("desktop-chore")
            .unwrap()
            .unwrap()
            .enabled,
        "the refused mixed call changed nothing"
    );
    run_tool_as_operator(&runtime, "orbit.auto_task.update", toggle.clone()).unwrap();
    assert!(
        run_tool_as_operator(&runtime, "orbit.auto_task.update", toggle).is_err(),
        "stale checked toggle refused"
    );
    let mut mint =
        json!({"workspace":repo,"name":"desktop-chore","acknowledge_unconditional":false});
    assert!(run_tool_as_operator(&runtime, "orbit.auto_task.mint", mint.clone()).is_err());
    mint["acknowledge_unconditional"] = json!(true);
    let minted = run_tool_as_operator(&runtime, "orbit.auto_task.mint", mint).unwrap();
    assert!(minted["task_id"].is_string());
    let runs = run_tool_as_operator(
        &runtime,
        "orbit.workflow.run.list",
        json!({"workspace":repo,"view":"bounded"}),
    )
    .unwrap();
    assert_eq!(runs["total"], 0, "mint creates a task without dispatching");
    use crate::application::job::pipeline::worker_command_override;
    worker_command_override::set(["sh", "-c", "sleep 1"]);
    let started = run_tool_as_operator(
        &runtime,
        "orbit.pipeline.invoke",
        json!({"workspace":repo,"default_input":true,"job_name":"desktop-maintenance"}),
    )
    .expect("submit no-input catalog job");
    worker_command_override::clear();
    let run = runtime
        .get_job_run_backend(started["run_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(run.job_id, "desktop-maintenance");
    assert!(
        run.input
            .as_ref()
            .is_some_and(|input| input.get("task_ids").is_none())
    );
    let job_path = jobs.join("desktop-maintenance.yaml");
    let yaml = std::fs::read_to_string(&job_path).unwrap();
    std::fs::write(&job_path, yaml.replace("state: enabled", "state: disabled")).unwrap();
    assert!(
        run_tool_as_operator(
            &runtime,
            "orbit.pipeline.invoke",
            json!({"workspace":repo,"default_input":true,"job_name":"desktop-maintenance"})
        )
        .is_err(),
        "disabled definition cannot dispatch"
    );
    let replica = runtime
        .clone()
        .with_coordination_write_owner(Some("other-host".into()));
    assert!(
        run_tool_as_operator(
            &replica,
            "orbit.auto_task.update",
            json!({"workspace":repo,"name":"desktop-chore","expected_enabled":false,"enabled":true})
        )
        .is_err()
    );
    assert!(
        !runtime
            .auto_task_show("desktop-chore")
            .unwrap()
            .unwrap()
            .enabled
    );
}

#[test]
fn desktop_drain_persists_bounded_window_and_explicit_completion_policy() {
    if !isolated("desktop_drain_persists_bounded_window_and_explicit_completion_policy") {
        return;
    }
    let _guard = unmanaged_tool_env_guard();
    use crate::application::job::pipeline::worker_command_override;
    worker_command_override::set(["sh", "-c", "sleep 1"]);
    for complete in [false, true] {
        let (_root, runtime, repo) = test_runtime();
        let jobs = runtime.paths().global_dir.join("resources/jobs");
        std::fs::create_dir_all(&jobs).unwrap();
        std::fs::write(jobs.join("workspace_auto_pipeline.yaml"),"schemaVersion: 2\nkind: Job\nmetadata:\n  name: workspace_auto_pipeline\nspec:\n  state: enabled\n  kind: workflow\n  max_active_runs: 1\n  steps:\n    - id: nap\n      spec:\n        type: deterministic\n        action: sleep\n        config: {}\n").unwrap();
        let result=run_tool_as_operator(&runtime,"orbit.workflow.auto",json!({"workspace":repo,"action":"start","for_seconds":1800,"concurrency":2,"complete":complete})).expect("submit drain");
        let run = runtime
            .get_job_run_backend(result["run_id"].as_str().unwrap())
            .unwrap()
            .unwrap();
        let input = run.input.unwrap();
        assert_eq!(input["for_seconds"], 1800);
        assert_eq!(input["max_active_leaf_runs"], 2);
        assert_eq!(
            input["completion"].as_str().unwrap_or("review"),
            if complete { "done" } else { "review" }
        );
        assert_eq!(
            result["completion"],
            if complete { "done" } else { "review" }
        );
    }
    worker_command_override::clear();
}

#[test]
fn domain_guarded_task_verbs_preserve_receipts_and_revision_guards() {
    if !isolated("domain_guarded_task_verbs_preserve_receipts_and_revision_guards") {
        return;
    }
    let _guard = unmanaged_tool_env_guard();
    let (_root, runtime, repo) = test_runtime();
    let request = json!({"workspace":repo,"model":"codex","request_id":"domain-create","title":"Domain capture","description":"Daily work","acceptance_criteria":["Observed proof"],"priority":"medium"});
    let created = runtime
        .run_tool("orbit.task.add", request.clone())
        .expect("guarded create");
    let retry = runtime
        .run_tool("orbit.task.add", request)
        .expect("guarded create replay");
    assert_eq!(retry["replayed"], true);
    assert_eq!(created["snapshot"]["task"]["status"], "proposed");
    assert_eq!(created["snapshot"]["task"]["created_by"], "codex");
    let id = created["snapshot"]["task"]["id"].clone();
    let snapshot = runtime
        .run_tool(
            "orbit.task.show",
            json!({"workspace":repo,"id":id,"snapshot":true}),
        )
        .expect("versioned snapshot");
    assert_eq!(snapshot["revision"], created["snapshot"]["revision"]);
    let comment = json!({"workspace":repo,"request_id":"domain-comment","id":id,"expected_revision":snapshot["revision"],"comment":"One durable comment"});
    let first = runtime
        .run_tool("orbit.task.update", comment.clone())
        .expect("comment");
    let second = runtime
        .run_tool("orbit.task.update", comment)
        .expect("comment replay");
    assert_eq!(second["replayed"], true);
    assert_eq!(
        first["snapshot"]["comments_total"],
        second["snapshot"]["comments_total"]
    );
    let stale = runtime.run_tool("orbit.task.update", json!({"workspace":repo,"request_id":"domain-edit-stale","id":id,"expected_revision":snapshot["revision"],"title":"Stale edit"})).expect("structured revision refusal");
    assert_eq!(stale["conflict"]["code"], "revision_conflict");
    assert_eq!(stale["snapshot"]["task"]["title"], "Domain capture");
    for input in [
        json!({"workspace":repo,"request_id":"mixed","id":id,"expected_revision":second["snapshot"]["revision"],"title":"bad","comment":"mixed"}),
        json!({"workspace":repo,"request_id":"status","id":id,"expected_revision":second["snapshot"]["revision"],"status":"done"}),
        json!({"workspace":repo,"request_id":"spoof","id":id,"expected_revision":second["snapshot"]["revision"],"title":"bad","actor":"human"}),
    ] {
        assert!(
            runtime.run_tool("orbit.task.update", input).is_err(),
            "mixed guarded writes must refuse before committing"
        );
    }
}

#[test]
fn domain_extensions_preserve_operator_authority_and_explicit_workspace_requirements() {
    if !isolated(
        "domain_extensions_preserve_operator_authority_and_explicit_workspace_requirements",
    ) {
        return;
    }
    let _guard = unmanaged_tool_env_guard();
    let (_root, runtime, repo) = test_runtime();
    for (name, input) in [
        (
            "orbit.workflow.auto",
            json!({"workspace":repo,"action":"status"}),
        ),
        (
            "orbit.workflow.auto",
            json!({"workspace":repo,"action":"start","for_seconds":60}),
        ),
        (
            "orbit.routine.control",
            json!({"workspace":repo,"action":"list"}),
        ),
        (
            "orbit.auto_task.update",
            json!({"workspace":repo,"name":"none","enabled":false,"expected_enabled":true}),
        ),
        (
            "orbit.auto_task.mint",
            json!({"workspace":repo,"name":"none","acknowledge_unconditional":true}),
        ),
        (
            "orbit.pipeline.invoke",
            json!({"workspace":repo,"job_name":"none","default_input":true}),
        ),
        (
            "orbit.pipeline.invoke",
            json!({"workspace":repo,"job_name":"none","input":{"task_ids":["TST-1"]}}),
        ),
        (
            "orbit.workflow.run.list",
            json!({"workspace":repo,"view":"bounded","include_catalog":true}),
        ),
        (
            "orbit.auto_task.list",
            json!({"workspace":repo,"view":"bounded"}),
        ),
    ] {
        let result = runtime.execute_tool_command_dispatch_with_session_context(
            name,
            input,
            None,
            None,
            ToolEntryPoint::Mcp,
            ToolSessionContext::default(),
        );
        assert!(
            matches!(result, Err(OrbitError::CapabilityDenied(_))),
            "{name}: {result:?}"
        );
    }
    for (name, input) in [
        ("orbit.task.list", json!({"view":"bounded"})),
        ("orbit.task.show", json!({"id":"TST-1","snapshot":true})),
        (
            "orbit.task.add",
            json!({"request_id":"no-workspace","title":"No implicit route","description":"","acceptance_criteria":["proof"]}),
        ),
        (
            "orbit.task.list",
            json!({"workspace":repo,"view":"bounded","fields":["title"]}),
        ),
        (
            "orbit.task.show",
            json!({"workspace":repo,"id":"TST-1","snapshot":true,"fields":["title"]}),
        ),
    ] {
        assert!(
            runtime.run_tool(name, input).is_err(),
            "{name} refuses an implicit destination or mixed projections"
        );
    }
    let status = run_tool_as_operator(
        &runtime,
        "orbit.workflow.auto",
        json!({"workspace":repo,"action":"status"}),
    )
    .expect("authorized observational readiness");
    assert_eq!(status["controls_authorized"], true);
    let catalog = run_tool_as_operator(
        &runtime,
        "orbit.workflow.run.list",
        json!({"workspace":repo,"view":"bounded","include_catalog":true}),
    )
    .expect("combined bounded runs and catalog");
    assert!(catalog["runs"]["items"].is_array());
    assert!(catalog["catalog"]["items"].is_array());
}

#[test]
fn guarded_task_update_review_cannot_inherit_completion_from_generic_update() {
    if !isolated("guarded_task_update_review_cannot_inherit_completion_from_generic_update") {
        return;
    }
    let _guard = unmanaged_tool_env_guard();
    let (_root, runtime, repo) = test_runtime();
    let runtime = runtime.with_actor(crate::ActorIdentity::human("fixture"));
    let task = runtime
        .add_task(crate::application::task::TaskAddParams {
            title: "Review fixture".into(),
            acceptance_criteria: vec!["verified behavior".into()],
            ..Default::default()
        })
        .expect("task");
    runtime
        .update_task(
            &task.id,
            crate::application::task::TaskUpdateParams {
                status: Some(orbit_types::task::TaskStatus::Review),
                execution_summary: Some("verified fixture".into()),
                ..Default::default()
            },
        )
        .expect("review fixture");
    let mut agent = ToolSessionContext::default();
    agent
        .effective_capabilities
        .insert(orbit_types::tool::McpCapability::Agent);
    let snapshot = runtime
        .desktop_task_snapshot(&task.id, &agent)
        .expect("snapshot");
    let request = json!({"workspace":repo,"request_id":"domain-review","id":task.id,"expected_revision":snapshot.revision,"complete":true,"verdict":{"decision":"accept","rationale":"Verified behavior","criteria":[{"criterion":"verified behavior","met":true,"evidence":["execution_summary"]}],"evidence":["execution_summary"],"expected_run_id":null,"expected_head":null}});
    let refused = runtime
        .execute_tool_command_dispatch_with_session_context(
            "orbit.task.update",
            request.clone(),
            None,
            None,
            ToolEntryPoint::Mcp,
            agent.clone(),
        )
        .expect("structured refusal before write")
        .value;
    assert_eq!(refused["mutation_applied"], false);
    assert_eq!(
        runtime.get_task(&task.id).expect("unchanged").status,
        orbit_types::task::TaskStatus::Review
    );
    let mut record = request.clone();
    record["request_id"] = json!("domain-record");
    record["complete"] = json!(false);
    let recorded = runtime
        .execute_tool_command_dispatch_with_session_context(
            "orbit.task.update",
            record,
            None,
            None,
            ToolEntryPoint::Mcp,
            agent,
        )
        .expect("agent records evidence without completing")
        .value;
    assert_eq!(recorded["snapshot"]["task"]["status"], "review");
    let mut complete = request;
    complete["expected_revision"] = recorded["snapshot"]["revision"].clone();
    let accepted = run_tool_as_operator(&runtime, "orbit.task.update", complete)
        .expect("operator may explicitly complete");
    assert_eq!(accepted["snapshot"]["task"]["status"], "done");
}
