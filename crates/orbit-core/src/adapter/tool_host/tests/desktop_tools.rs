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
            .run_tool("orbit.desktop.read", json!({"scope":"tasks"}))
            .is_err()
    );
    let result = runtime.run_tool("orbit.desktop.task.write", json!({"workspace":repo,"request_id":"spoof","operation":{"kind":"create","title":"No spoof","description":"","acceptance_criteria":["Proof"]},"actor":"human"}));
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
        "orbit.desktop.read",
        json!({"workspace":repo,"scope":"runs"}),
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
        "orbit.desktop.read",
        json!({"workspace":repo,"scope":"runs"}),
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
    let request = json!({"workspace":repo,"model":"codex","request_id":"create-1","operation":{"kind":"create","title":"Capture","description":"Daily work","acceptance_criteria":["Observed proof"],"priority":"medium"}});
    let created = runtime
        .run_tool("orbit.desktop.task.write", request.clone())
        .expect("create proposed task");
    let retried = runtime
        .run_tool("orbit.desktop.task.write", request.clone())
        .expect("reconcile create reply");
    assert_eq!(
        created["snapshot"]["task"]["id"],
        retried["snapshot"]["task"]["id"]
    );
    assert_eq!(created["snapshot"]["task"]["status"], "proposed");
    let mut changed = request;
    changed["operation"]["title"] = json!("Changed request");
    let refused = runtime
        .run_tool("orbit.desktop.task.write", changed)
        .expect("definite precommit refusal");
    assert_eq!(refused["mutation_applied"], false);
    assert!(refused["refusal"]["message"].is_string());
    let id = created["snapshot"]["task"]["id"].clone();
    let comment = json!({"workspace":repo,"model":"codex","request_id":"comment-1","operation":{"kind":"comment","id":id,"expected_revision":retried["snapshot"]["revision"],"comment":"One durable comment"}});
    let first = runtime
        .run_tool("orbit.desktop.task.write", comment.clone())
        .expect("comment");
    let second = runtime
        .run_tool("orbit.desktop.task.write", comment)
        .expect("same comment retry");
    assert_eq!(
        first["snapshot"]["comments_total"],
        second["snapshot"]["comments_total"]
    );
    assert_eq!(second["replayed"], true);
    let conflict = runtime.run_tool("orbit.desktop.task.write", json!({"workspace":repo,"request_id":"stale-1","operation":{"kind":"edit","id":id,"expected_revision":created["snapshot"]["revision"],"fields":{"title":"stale"}}})).expect("structured stale response");
    assert_eq!(conflict["conflict"]["code"], "revision_conflict");
    assert_eq!(conflict["snapshot"]["task"]["title"], "Capture");
    assert_eq!(
        conflict["snapshot"]["revision"],
        second["snapshot"]["revision"]
    );
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
            "orbit.desktop.drain",
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
        "orbit.desktop.read",
        json!({"workspace":repo,"scope":"drain"}),
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
        assert!(run_tool_as_operator(&runtime, "orbit.desktop.drain", input).is_err());
    }
    assert!(
        run_tool_as_operator(
            &runtime,
            "orbit.desktop.drain",
            json!({"workspace":repo,"action":"stop","complete":true})
        )
        .is_err()
    );
    let runs = run_tool_as_operator(
        &runtime,
        "orbit.desktop.read",
        json!({"workspace":repo,"scope":"runs"}),
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
        "orbit.desktop.read",
        json!({"workspace":repo,"scope":"drain"}),
    )
    .expect("readiness");
    assert_eq!(read["schema_version"], 1);
    assert_eq!(read["snapshot"]["read_only"], true);
    assert!(read["capacity"].is_object());
    assert_eq!(read["controls_authorized"], true);
    let stopped = run_tool_as_operator(
        &runtime,
        "orbit.desktop.drain",
        json!({"workspace":repo,"action":"stop"}),
    )
    .expect("idle stop");
    assert_eq!(stopped["outcome"], "idle");
    assert_eq!(stopped["coordinators"], json!([]));
    let runs = run_tool_as_operator(
        &runtime,
        "orbit.desktop.read",
        json!({"workspace":repo,"scope":"runs"}),
    )
    .expect("runs");
    assert_eq!(runs["total"], 0);
}

#[test]
fn desktop_automation_scopes_definitions_checks_conflicts_and_mints_without_dispatch() {
    if !isolated(
        "desktop_automation_scopes_definitions_checks_conflicts_and_mints_without_dispatch",
    ) {
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
    // Anonymous sessions cannot inspect operator run/schedule state or mutate it.
    for scope in ["routines", "auto_tasks", "jobs"] {
        let denied = runtime.execute_tool_command_dispatch_with_session_context(
            "orbit.desktop.read",
            json!({"workspace":repo,"scope":scope}),
            None,
            None,
            ToolEntryPoint::Mcp,
            ToolSessionContext::default(),
        );
        assert!(matches!(denied, Err(OrbitError::CapabilityDenied(_))));
        let data = run_tool_as_operator(
            &runtime,
            "orbit.desktop.read",
            json!({"workspace":repo,"scope":scope,"limit":1}),
        )
        .expect("read definitions");
        assert!(data["items"].as_array().unwrap().len() <= 1);
    }
    let read = run_tool_as_operator(
        &runtime,
        "orbit.desktop.read",
        json!({"workspace":repo,"scope":"routines"}),
    )
    .unwrap();
    assert_eq!(read["items"][0]["name"], "desktop-routine", "{read}");
    let mut toggle = json!({"workspace":repo,"action":"toggle","kind":"routine","name":"desktop-routine","target":"job:desktop-maintenance","expected_enabled":true,"enabled":false});
    let denied = runtime.execute_tool_command_dispatch_with_session_context(
        "orbit.desktop.automation",
        toggle.clone(),
        None,
        None,
        ToolEntryPoint::Mcp,
        ToolSessionContext::default(),
    );
    assert!(matches!(denied, Err(OrbitError::CapabilityDenied(_))));
    toggle["target"] = json!("job:changed-target");
    assert!(run_tool_as_operator(&runtime, "orbit.desktop.automation", toggle.clone()).is_err());
    toggle["target"] = json!("job:desktop-maintenance");
    assert_eq!(
        run_tool_as_operator(&runtime, "orbit.desktop.automation", toggle.clone()).unwrap()["enabled"],
        false
    );
    assert!(
        run_tool_as_operator(&runtime, "orbit.desktop.automation", toggle).is_err(),
        "stale toggle refused"
    );
    let toggle = json!({"workspace":repo,"action":"toggle","kind":"auto_task","name":"desktop-chore","expected_enabled":true,"enabled":false});
    run_tool_as_operator(&runtime, "orbit.desktop.automation", toggle.clone()).unwrap();
    assert!(run_tool_as_operator(&runtime, "orbit.desktop.automation", toggle).is_err());
    let mut mint = json!({"workspace":repo,"action":"mint","kind":"auto_task","name":"desktop-chore","acknowledge_unconditional":false});
    assert!(run_tool_as_operator(&runtime, "orbit.desktop.automation", mint.clone()).is_err());
    mint["acknowledge_unconditional"] = json!(true);
    let minted = run_tool_as_operator(&runtime, "orbit.desktop.automation", mint).unwrap();
    assert!(minted["task_id"].is_string());
    let runs = run_tool_as_operator(
        &runtime,
        "orbit.desktop.read",
        json!({"workspace":repo,"scope":"runs"}),
    )
    .unwrap();
    assert_eq!(runs["total"], 0, "mint creates a task without dispatching");
    use crate::application::job::pipeline::worker_command_override;
    worker_command_override::set(["sh", "-c", "sleep 1"]);
    let started = run_tool_as_operator(
        &runtime,
        "orbit.desktop.automation",
        json!({"workspace":repo,"kind":"job","action":"run","name":"desktop-maintenance"}),
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
            "orbit.desktop.automation",
            json!({"workspace":repo,"kind":"job","action":"run","name":"desktop-maintenance"})
        )
        .is_err(),
        "disabled definition cannot dispatch"
    );
    let replica = runtime
        .clone()
        .with_coordination_write_owner(Some("other-host".into()));
    assert!(run_tool_as_operator(&replica,"orbit.desktop.automation",json!({"workspace":repo,"action":"toggle","kind":"auto_task","name":"desktop-chore","expected_enabled":false,"enabled":true})).is_err());
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
        let result=run_tool_as_operator(&runtime,"orbit.desktop.drain",json!({"workspace":repo,"action":"start","for_seconds":1800,"concurrency":2,"complete":complete})).expect("submit drain");
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
