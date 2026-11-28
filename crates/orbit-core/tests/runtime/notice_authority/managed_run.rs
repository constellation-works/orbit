//! Managed-run notice authority and fail-closed rejection scenarios.

#[cfg(unix)]
use std::path::Path;

use chrono::Utc;
use orbit_common::Env;
use orbit_core::adapter::command::{ToolCommandCall, ToolEntryPoint};
use orbit_core::app::resident;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::runtime::OrbitRuntime;
use orbit_engine::RunLifecycle as _;
use orbit_types::task::{ExecutionLocation, TaskArtifact};
use orbit_types::tool::{McpCapability, McpTransport, ToolSessionContext, WorkerInvocation};
use serde_json::{Value, json};

#[cfg(unix)]
use super::FIXTURE_CLAUDE_WORKER_TOKEN;
use super::{add_task, call, refused, runtime_at, runtime_at_roots, session};

fn local_cli_session(runtime: &OrbitRuntime) -> ToolSessionContext {
    ToolSessionContext {
        workspace: Some(runtime.paths().repo_root.to_string_lossy().into_owned()),
        workspace_id: runtime.workspace_id().ok(),
        transport: Some(McpTransport::Local),
        ..ToolSessionContext::default()
    }
}

fn call_with_env(
    runtime: &OrbitRuntime,
    env: &Env,
    tool: &str,
    input: Value,
    entry_point: ToolEntryPoint,
    session_context: ToolSessionContext,
) -> Result<Value, String> {
    orbit_core::test_support::with_runtime_env(runtime.clone(), env.clone())
        .execute_tool_command_dispatch_with_session_context_and_env(
            env,
            ToolCommandCall {
                name: tool,
                input,
                agent_override: None,
                model_override: None,
                entry_point,
                session_context,
            },
        )
        .map(|outcome| outcome.value)
        .map_err(|error| error.to_string())
}

fn managed_pipeline_env(run_id: &str, task_id: Option<&str>) -> Env {
    let mut vars = vec![
        ("ORBIT_MANAGED_RUN_CONTEXT", "1"),
        ("ORBIT_RUN_ID", run_id),
        ("ORBIT_AGENT_NAME", "claude"),
        ("ORBIT_AGENT_MODEL", "claude-opus-5-5"),
        ("ORBIT_TASK_ACTOR_KIND", "agent"),
    ];
    if let Some(task_id) = task_id {
        vars.push(("ORBIT_TASK_ID", task_id));
        vars.push(("ORBIT_ACTIVE_TASK_ID", task_id));
    }
    Env::from_vars(vars)
}

fn insert_run(runtime: &OrbitRuntime, job_id: &str, input: Option<Value>) -> String {
    orbit_core::test_support::EngineHost::new(runtime)
        .insert_job_run(job_id, 1, Utc::now(), input, None)
        .unwrap()
        .run_id
}

fn add_resident_task(runtime: &OrbitRuntime, title: &str, watched: &str) -> String {
    let id = runtime
        .add_task(TaskAddParams {
            title: title.into(),
            tags: vec!["resident".into()],
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    let mandate = json!({
        "schema": "resident.mandate/v1",
        "goal": format!("Deliver {title}"),
        "exit_criteria": ["Done"],
        "workspaces": [runtime.workspace_id().unwrap()],
        "watch": {"tasks": [watched], "tags": []},
        "crew": "sol",
        "cadence": {"floor_minutes": 10, "heartbeat_minutes": 120}
    });
    runtime
        .update_task_as_human(
            &id,
            TaskUpdateParams {
                upsert_artifacts: vec![TaskArtifact::from_text(
                    "resident/mandate.json",
                    mandate.to_string(),
                )],
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
    id
}

#[cfg(unix)]
/// Capture the exact `ORBIT_*` environment that `run_cli_backend` stamps for a
/// managed `agent_implement` run of `task_id`.
fn dispatched_agent_implement_env(
    runtime: &OrbitRuntime,
    root: &Path,
    run_id: &str,
    task_id: Option<&str>,
    activity_input: Value,
) -> Env {
    use std::os::unix::fs::PermissionsExt as _;

    let _credential = orbit_common::test_env::scoped([(
        "CLAUDE_CODE_OAUTH_TOKEN",
        Some(FIXTURE_CLAUDE_WORKER_TOKEN),
    )]);
    let recorded = root.join("agent_implement.env");
    let bin = root.join("agent_implement-env-provider");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\ncat > /dev/null\nenv | grep '^ORBIT_' > '{}'\nprintf '%s\\n' '{{\"type\":\"result\",\"is_error\":false,\"result\":\"{{\\\"schemaVersion\\\":1,\\\"status\\\":\\\"success\\\",\\\"result\\\":{{}},\\\"error\\\":null}}\"}}'\n",
            recorded.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();

    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let mut executor: orbit_types::resource::ExecutorResource = serde_yaml::from_str(
        &std::fs::read_to_string(assets.join("executors/claude.yaml")).unwrap(),
    )
    .unwrap();
    executor.spec.command = Some(bin.display().to_string());
    executor.spec.sandbox = Some(orbit_types::workflow::ExecutorSandboxKind::Off);
    runtime
        .upsert_executor_def(&orbit_types::workflow::ExecutorDef::from_resource_spec(
            executor.metadata.name,
            executor.spec,
            None,
            None,
        ))
        .unwrap();

    let yaml = std::fs::read_to_string(assets.join("activities/agent_implement.yaml")).unwrap();
    let asset = orbit_engine::activity_job::load_activity_asset(&yaml).unwrap();
    let orbit_types::workflow::ActivityV2Spec::AgentLoop(mut spec) = asset.spec.spec else {
        panic!("agent_implement activity")
    };
    spec.provider = orbit_types::workflow::activity_job::Provider::Claude;

    let audit = orbit_engine::V2AuditWriter::with_disk_sinks(
        &root.join("audit-managed-env"),
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
        run_id,
        "fixture",
        None,
    )
    .unwrap();
    orbit_engine::activity_job::cli_runner::run_cli_backend(
        &orbit_core::test_support::EngineHost::new(runtime),
        &spec,
        "agent_implement",
        run_id,
        audit,
        &activity_input,
        None,
    )
    .unwrap();

    let raw = std::fs::read_to_string(&recorded)
        .unwrap_or_else(|error| panic!("agent_implement provider never recorded env: {error}"));
    let vars: Vec<(String, String)> = raw
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let env = Env::from_vars(vars);
    assert_eq!(
        env.orbit_managed_run_context()
            .and_then(std::ffi::OsStr::to_str),
        Some("1")
    );
    assert_eq!(
        env.orbit_run_id().and_then(std::ffi::OsStr::to_str),
        Some(run_id)
    );
    assert_eq!(
        env.orbit_task_id().and_then(std::ffi::OsStr::to_str),
        task_id
    );
    env
}

pub(super) fn managed_pipeline_run_posts_and_closes_notices_as_agent() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let task = add_task(&runtime, "Managed pipeline task");
    let other_task = add_task(&runtime, "Other task in workspace");
    let run_id = insert_run(
        &runtime,
        "task_pr_pipeline",
        Some(json!({"task_ids": [task]})),
    );

    #[cfg(unix)]
    let cli_env = dispatched_agent_implement_env(
        &runtime,
        root.path(),
        &run_id,
        Some(&task),
        json!({"task_id": task, "workspace_path": root.path().join("repo")}),
    );
    #[cfg(not(unix))]
    let cli_env = managed_pipeline_env(&run_id, Some(&task));

    // Local `orbit tool run` path (transport: Local, no worker binding).
    let posted = call_with_env(
        &runtime,
        &cli_env,
        "orbit.notice.post",
        json!({
            "kind": "incident",
            "title": "Shared test fixture broken",
            "linked_task": task
        }),
        ToolEntryPoint::Cli,
        local_cli_session(&runtime),
    )
    .expect("managed CLI run must post as an Agent for its own task");
    assert_eq!(posted["outcome"], "created");
    assert_eq!(posted["notice"]["author"]["class"], "agent");
    assert_eq!(posted["notice"]["author"]["task_id"], task);
    assert_eq!(posted["notice"]["author"]["run_id"], run_id);
    let cli_notice_id = posted["notice"]["id"].as_str().unwrap().to_string();

    // Agent restrictions remain enforced on the local managed run.
    refused(
        call_with_env(
            &runtime,
            &cli_env,
            "orbit.notice.post",
            json!({
                "kind": "incident",
                "title": "Global attempt",
                "scope": "global",
                "linked_task": task
            }),
            ToolEntryPoint::Cli,
            local_cli_session(&runtime),
        ),
        "notice_scope_refused",
    );
    refused(
        call_with_env(
            &runtime,
            &cli_env,
            "orbit.notice.post",
            json!({
                "kind": "maintenance",
                "title": "Maintenance attempt",
                "linked_task": task
            }),
            ToolEntryPoint::Cli,
            local_cli_session(&runtime),
        ),
        "notice_kind_refused",
    );

    // Managed run may close its own task's notice.
    let closed = call_with_env(
        &runtime,
        &cli_env,
        "orbit.notice.close",
        json!({"id": cli_notice_id, "reason": "cleared"}),
        ToolEntryPoint::Cli,
        local_cli_session(&runtime),
    )
    .expect("managed run may close its own notice");
    assert_eq!(closed["notice"]["status"], "closed");
    assert_eq!(closed["notice"]["closed"]["by"]["class"], "agent");
    assert_eq!(closed["notice"]["closed"]["by"]["task_id"], task);

    // Managed MCP session path (Agent capability, no worker binding).
    let mcp_env = managed_pipeline_env(&run_id, Some(&task));
    let mcp_posted = call_with_env(
        &runtime,
        &mcp_env,
        "orbit.notice.post",
        json!({
            "kind": "info",
            "title": "Managed MCP post",
            "evidence": [{"type": "run", "value": run_id}]
        }),
        ToolEntryPoint::Mcp,
        session(McpCapability::Agent),
    )
    .expect("managed MCP run must post as an Agent for its own task");
    assert_eq!(mcp_posted["notice"]["author"]["class"], "agent");
    assert_eq!(mcp_posted["notice"]["author"]["task_id"], task);
    assert_eq!(mcp_posted["notice"]["author"]["run_id"], run_id);

    // A singular-`task_id` recovery pipeline run (`blocked_task_recovery_pipeline`)
    // also resolves as an Agent poster for its bound task.
    let recovery_run = insert_run(
        &runtime,
        "blocked_task_recovery_pipeline",
        Some(json!({"task_id": other_task})),
    );
    let recovery_env = managed_pipeline_env(&recovery_run, Some(&other_task));
    let recovery_posted = call_with_env(
        &runtime,
        &recovery_env,
        "orbit.notice.post",
        json!({
            "kind": "info",
            "title": "Recovery run report",
            "linked_task": other_task
        }),
        ToolEntryPoint::Cli,
        local_cli_session(&runtime),
    )
    .expect("singular task_id managed run must post as an Agent");
    assert_eq!(recovery_posted["notice"]["author"]["class"], "agent");
    assert_eq!(recovery_posted["notice"]["author"]["task_id"], other_task);
    assert_eq!(recovery_posted["notice"]["author"]["run_id"], recovery_run);
}

pub(super) fn managed_run_notice_authority_preserves_fail_closed_rejections() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let home = runtime_at_roots(&global, &root.path().join("home-ws/.orbit"));
    let other_ws = runtime_at_roots(&global, &root.path().join("other-ws/.orbit"));
    let home_ws_id = home.workspace_id().unwrap();
    let other_ws_id = other_ws.workspace_id().unwrap();
    assert_ne!(home_ws_id, other_ws_id);

    let task = add_task(&home, "Home bound task");
    let sibling_task = add_task(&home, "Another home task");
    let foreign_task = add_task(&other_ws, "Task in another workspace");
    let mandate_a = add_resident_task(&home, "Resident mandate A", &task);
    let mandate_b = add_resident_task(&home, "Resident mandate B", &sibling_task);

    let valid_run = insert_run(&home, "task_pr_pipeline", Some(json!({"task_ids": [task]})));
    let sibling_run = insert_run(
        &home,
        "task_pr_pipeline",
        Some(json!({"task_ids": [sibling_task]})),
    );
    let unscoped_run = insert_run(&home, "workspace_auto_pipeline", Some(json!({})));
    let other_ws_run = insert_run(
        &other_ws,
        "task_pr_pipeline",
        Some(json!({"task_ids": [task]})),
    );
    let foreign_task_run = insert_run(
        &home,
        "task_pr_pipeline",
        Some(json!({"task_ids": [foreign_task]})),
    );
    let resident_run = insert_run(
        &home,
        "resident_cycle_pipeline",
        Some(json!({"task_ids": [mandate_a]})),
    );
    let cycle = resident::begin_resident_cycle(&home, &resident_run, Utc::now()).unwrap();
    assert_eq!(cycle["cycle_count"], 1, "{cycle}");

    let post_for = |linked: &str| {
        json!({
            "kind": "info",
            "title": format!("Probe for {linked}"),
            "linked_task": linked
        })
    };

    // 1. A session with no worker binding and no run/task scope is refused.
    refused(
        call_with_env(
            &home,
            &Env::default(),
            "orbit.notice.post",
            post_for(&task),
            ToolEntryPoint::Mcp,
            session(McpCapability::Agent),
        ),
        "notice_poster_unidentified",
    );

    // 2. A run id that is not a managed run of the named task is refused:
    //    missing run, run of a sibling task, taskless run, run in another
    //    workspace, and a non-resident run omitting ORBIT_TASK_ID.
    for bad_env in [
        managed_pipeline_env("jrun-does-not-exist", Some(&task)),
        managed_pipeline_env(&sibling_run, Some(&task)),
        managed_pipeline_env(&unscoped_run, Some(&task)),
        managed_pipeline_env(&other_ws_run, Some(&task)),
        managed_pipeline_env(&valid_run, None),
    ] {
        refused(
            call_with_env(
                &home,
                &bad_env,
                "orbit.notice.post",
                post_for(&task),
                ToolEntryPoint::Cli,
                local_cli_session(&home),
            ),
            "notice_poster_unidentified",
        );
    }

    // 3. A task not in this workspace is refused, whether via a local managed
    //    run or a worker binding.
    refused(
        call_with_env(
            &home,
            &managed_pipeline_env(&foreign_task_run, Some(&foreign_task)),
            "orbit.notice.post",
            json!({
                "kind": "info",
                "title": "Foreign task managed run",
                "evidence": [{"type": "run", "value": foreign_task_run}]
            }),
            ToolEntryPoint::Cli,
            local_cli_session(&home),
        ),
        "notice_poster_unidentified",
    );
    let foreign_task_worker = ToolSessionContext {
        worker_invocation: Some(WorkerInvocation {
            owner_machine_id: "owner-machine".into(),
            owner_workspace_id: home_ws_id.clone(),
            owner_destination: "owner".into(),
            task_id: foreign_task.clone(),
            claim_id: "claim-foreign-task".into(),
            execution: ExecutionLocation {
                machine_id: "leaf-machine".into(),
                machine_name: None,
            },
            bound_run_id: "jrun-foreign-task".into(),
        }),
        ..session(McpCapability::Agent)
    };
    refused(
        call(
            &home,
            "orbit.notice.post",
            post_for(&task),
            &foreign_task_worker,
        ),
        "notice_poster_unidentified",
    );

    // 4. A worker binding whose owner workspace differs is refused with
    //    `notice_scope_refused`.
    let mismatched_ws_worker = ToolSessionContext {
        worker_invocation: Some(WorkerInvocation {
            owner_machine_id: "owner-machine".into(),
            owner_workspace_id: other_ws_id,
            owner_destination: "owner".into(),
            task_id: task.clone(),
            claim_id: "claim-other-ws".into(),
            execution: ExecutionLocation {
                machine_id: "leaf-machine".into(),
                machine_name: None,
            },
            bound_run_id: "jrun-other-ws".into(),
        }),
        ..session(McpCapability::Agent)
    };
    refused(
        call(
            &home,
            "orbit.notice.post",
            post_for(&task),
            &mismatched_ws_worker,
        ),
        "notice_scope_refused",
    );

    // 5. A resident run whose mandate is another task is refused.
    for wrong_task in [&mandate_b, &task] {
        refused(
            call_with_env(
                &home,
                &managed_pipeline_env(&resident_run, Some(wrong_task)),
                "orbit.notice.post",
                json!({
                    "kind": "warn",
                    "title": "Wrong mandate attempt",
                    "linked_task": wrong_task
                }),
                ToolEntryPoint::Cli,
                local_cli_session(&home),
            ),
            "notice_poster_unidentified",
        );
    }
    // The resident run still posts for its own mandate (with or without ORBIT_TASK_ID).
    let resident_posted = call_with_env(
        &home,
        &managed_pipeline_env(&resident_run, None),
        "orbit.notice.post",
        json!({"kind": "warn", "title": "Mandate A needs operator"}),
        ToolEntryPoint::Mcp,
        session(McpCapability::Agent),
    )
    .expect("resident run posts for its own mandate");
    assert_eq!(resident_posted["notice"]["author"]["class"], "agent");
    assert_eq!(resident_posted["notice"]["author"]["task_id"], mandate_a);
    assert_eq!(resident_posted["notice"]["author"]["run_id"], resident_run);

    // 6. An orchestrator attribution naming no configured crew is refused.
    refused(
        call_with_env(
            &home,
            &Env::default(),
            "orbit.notice.post",
            json!({
                "kind": "info",
                "title": "Unknown crew",
                "orchestrator": "not-a-configured-crew"
            }),
            ToolEntryPoint::Mcp,
            session(McpCapability::Agent),
        ),
        "notice_poster_unidentified",
    );

    // 7. A request-supplied orchestrator cannot override a managed binding
    //    (whether valid or invalid).
    let valid_env = managed_pipeline_env(&valid_run, Some(&task));
    refused(
        call_with_env(
            &home,
            &valid_env,
            "orbit.notice.post",
            json!({
                "kind": "incident",
                "title": "Orchestrator scope override attempt",
                "scope": "global",
                "linked_task": task,
                "orchestrator": "sol"
            }),
            ToolEntryPoint::Cli,
            local_cli_session(&home),
        ),
        "notice_scope_refused",
    );
    refused(
        call_with_env(
            &home,
            &valid_env,
            "orbit.notice.post",
            json!({
                "kind": "maintenance",
                "title": "Orchestrator kind override attempt",
                "linked_task": task,
                "orchestrator": "sol"
            }),
            ToolEntryPoint::Mcp,
            session(McpCapability::Agent),
        ),
        "notice_kind_refused",
    );
    let kept_agent = call_with_env(
        &home,
        &valid_env,
        "orbit.notice.post",
        json!({
            "kind": "info",
            "title": "Managed run ignores orchestrator field",
            "linked_task": task,
            "orchestrator": "sol"
        }),
        ToolEntryPoint::Cli,
        local_cli_session(&home),
    )
    .expect("managed run stays an Agent even when input passes orchestrator");
    assert_eq!(kept_agent["notice"]["author"]["class"], "agent");
    assert_eq!(kept_agent["notice"]["author"]["task_id"], task);
    assert_eq!(kept_agent["notice"]["author"]["run_id"], valid_run);

    // An invalid managed run binding also must not fall through to orchestrator.
    refused(
        call_with_env(
            &home,
            &managed_pipeline_env(&sibling_run, Some(&task)),
            "orbit.notice.post",
            json!({
                "kind": "info",
                "title": "Invalid binding must not fall through",
                "orchestrator": "sol"
            }),
            ToolEntryPoint::Mcp,
            session(McpCapability::Agent),
        ),
        "notice_poster_unidentified",
    );

    // Ensure the fixture config's configured crew ("sol") really works when unbound.
    let unbound_orch = call_with_env(
        &home,
        &Env::default(),
        "orbit.notice.post",
        json!({
            "kind": "info",
            "title": "Unbound orchestrator works",
            "orchestrator": "sol"
        }),
        ToolEntryPoint::Mcp,
        session(McpCapability::Agent),
    )
    .expect("unbound orchestrator with configured crew still works");
    assert_eq!(unbound_orch["notice"]["author"]["class"], "orchestrator");
    assert_eq!(unbound_orch["notice"]["author"]["crew"], "sol");
}

#[cfg(unix)]
pub(super) fn bundled_activity_notice_uses_selected_task_and_refuses_ambiguity() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let first = add_task(&runtime, "First bundled task");
    let active = add_task(&runtime, "Active bundled task");
    let run = insert_run(
        &runtime,
        "task_pr_pipeline",
        Some(json!({"task_ids": [first, active]})),
    );
    let env = dispatched_agent_implement_env(
        &runtime,
        root.path(),
        &run,
        Some(&active),
        json!({
            "task_ids": [first, active],
            "task_id": active,
            "workspace_path": root.path().join("repo")
        }),
    );
    let post = |env: &Env, title: &str| {
        call_with_env(
            &runtime,
            env,
            "orbit.notice.post",
            json!({"kind": "info", "title": title, "linked_task": active}),
            ToolEntryPoint::Cli,
            local_cli_session(&runtime),
        )
    };
    let notice = post(&env, "Selected activity").unwrap();
    assert_eq!(notice["notice"]["author"]["task_id"], active);
    let id = notice["notice"]["id"].as_str().unwrap();
    let mismatched = Env::from_vars([
        ("ORBIT_MANAGED_RUN_CONTEXT", "1"),
        ("ORBIT_RUN_ID", run.as_str()),
        ("ORBIT_TASK_ID", first.as_str()),
        ("ORBIT_ACTIVE_TASK_ID", active.as_str()),
    ]);
    refused(
        post(&mismatched, "Wrong task"),
        "notice_poster_unidentified",
    );
    refused(
        call_with_env(
            &runtime,
            &mismatched,
            "orbit.notice.close",
            json!({"id": id, "reason": "cleared"}),
            ToolEntryPoint::Cli,
            local_cli_session(&runtime),
        ),
        "notice_poster_unidentified",
    );
    let ambiguous = dispatched_agent_implement_env(
        &runtime,
        root.path(),
        &run,
        None,
        json!({"task_ids": [first, active], "workspace_path": root.path().join("repo")}),
    );
    refused(
        post(&ambiguous, "Ambiguous activity"),
        "notice_poster_unidentified",
    );
    refused(
        call_with_env(
            &runtime,
            &ambiguous,
            "orbit.notice.close",
            json!({"id": id, "reason": "cleared"}),
            ToolEntryPoint::Cli,
            local_cli_session(&runtime),
        ),
        "notice_poster_unidentified",
    );
    let closed = call_with_env(
        &runtime,
        &env,
        "orbit.notice.close",
        json!({"id": id, "reason": "cleared"}),
        ToolEntryPoint::Cli,
        local_cli_session(&runtime),
    )
    .unwrap();
    assert_eq!(closed["notice"]["closed"]["by"]["task_id"], active);
}
