//! Managed-run notice authority and fail-closed rejection scenarios.

#[cfg(unix)]
use std::path::Path;

use chrono::Utc;
use orbit_common::Env;
use orbit_core::adapter::command::{ToolCommandCall, ToolEntryPoint};
use orbit_core::app::resident;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::runtime::OrbitRuntime;
use orbit_engine::HostEnvironment as _;
use orbit_engine::RunLifecycle as _;
use orbit_tools::{ActivityBinding, ToolContext};
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
/// managed `agent_implement` run of `task_id`. `inherited` is set in the
/// launching process first, as an outer shell or worker would hold it.
fn dispatched_agent_implement_env(
    runtime: &OrbitRuntime,
    root: &Path,
    run_id: &str,
    task_id: Option<&str>,
    activity_input: Value,
    inherited: &[(&str, &str)],
) -> Env {
    use std::os::unix::fs::PermissionsExt as _;

    // One guard per thread: the credential and any inherited identity share it.
    let mut parent = vec![("CLAUDE_CODE_OAUTH_TOKEN", Some(FIXTURE_CLAUDE_WORKER_TOKEN))];
    parent.extend(inherited.iter().map(|(name, value)| (*name, Some(*value))));
    let _parent = orbit_common::test_env::scoped(parent);
    // The engine host reads the runtime's environment snapshot, taken when the
    // runtime opened, so the launching process's variables must be re-applied.
    let launching =
        orbit_core::test_support::with_runtime_env(runtime.clone(), orbit_common::Env::capture());
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
        &orbit_core::test_support::EngineHost::new(&launching),
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
    refused(
        call_with_env(
            &home,
            &managed_pipeline_env(&resident_run, None),
            "orbit.notice.post",
            json!({"kind": "warn", "title": "Unattested resident"}),
            ToolEntryPoint::Mcp,
            session(McpCapability::Agent),
        ),
        "notice_poster_unidentified",
    );
    let resident_posted = call_with_activity(
        &home,
        "orbit.notice.post",
        json!({"kind": "warn", "title": "Mandate A needs operator"}),
        activity(&resident_run, None),
    )
    .expect("the attested resident run resolves its own mandate");
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

    // A managed call without a host binding cannot fall back to an operator
    // session or to orchestrator attribution in the request.
    for (entry, caller) in [
        (ToolEntryPoint::Cli, local_cli_session(&home)),
        (ToolEntryPoint::Mcp, session(McpCapability::Agent)),
        (ToolEntryPoint::Mcp, session(McpCapability::Operator)),
    ] {
        for env in [
            managed_pipeline_env(&valid_run, Some(&task)),
            Env::from_vars([("ORBIT_MANAGED_RUN_CONTEXT", "1")]),
        ] {
            refused(
                call_with_env(
                    &home,
                    &env,
                    "orbit.notice.post",
                    json!({
                        "kind": "info",
                        "title": "Unattested activity",
                        "linked_task": task,
                        "orchestrator": "sol"
                    }),
                    entry,
                    caller.clone(),
                ),
                "notice_poster_unidentified",
            );
        }
    }

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
pub(super) fn ambiguous_bundle_dispatch_drops_inherited_task_identity() {
    crate::scenarios::require_isolated_mutable_fixture(
        "ambiguous_bundle_dispatch_drops_inherited_task_identity",
    );
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let first = add_task(&runtime, "First inherited bundled task");
    let second = add_task(&runtime, "Second bundled task");
    let run = insert_run(
        &runtime,
        "task_pr_pipeline",
        Some(json!({"task_ids": [first, second]})),
    );
    let notice = call_with_activity(
        &runtime,
        "orbit.notice.post",
        json!({"kind": "info", "title": "Selected bundled activity", "linked_task": second}),
        activity(&run, Some(&second)),
    )
    .unwrap();
    let id = notice["notice"]["id"].as_str().unwrap();

    // The launching process holds a member task's identity. The ambiguous
    // bundle has no singular task, so the provider must receive neither name.
    let ambiguous = dispatched_agent_implement_env(
        &runtime,
        root.path(),
        &run,
        None,
        json!({"task_ids": [first, second], "workspace_path": root.path().join("repo")}),
        &[
            ("ORBIT_TASK_ID", first.as_str()),
            ("ORBIT_ACTIVE_TASK_ID", first.as_str()),
        ],
    );
    assert!(ambiguous.var("ORBIT_TASK_ID").is_none());
    assert!(ambiguous.var("ORBIT_ACTIVE_TASK_ID").is_none());
    refused(
        call_with_env(
            &runtime,
            &ambiguous,
            "orbit.notice.post",
            json!({"kind": "info", "title": "Inherited identity", "linked_task": first}),
            ToolEntryPoint::Cli,
            local_cli_session(&runtime),
        ),
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
}

/// Call `tool` in process under the activity binding a dispatching host
/// attested, with no managed environment for the notice flow to re-read.
fn call_with_activity(
    runtime: &OrbitRuntime,
    tool: &str,
    input: Value,
    activity: Option<ActivityBinding>,
) -> Result<Value, String> {
    orbit_core::test_support::EngineHost::new(runtime)
        .run_tool_with_context(
            tool,
            input,
            ToolContext {
                session_context: session(McpCapability::Agent),
                activity_binding: activity,
                ..ToolContext::default()
            },
        )
        .map_err(|error| error.to_string())
}

fn activity(run_id: &str, task_id: Option<&str>) -> Option<ActivityBinding> {
    Some(ActivityBinding {
        job_run_id: run_id.to_string(),
        task_id: task_id.map(ToOwned::to_owned),
    })
}

/// A notice's record and history, and the open notice count, as the
/// operator reads them.
fn notice_state(runtime: &OrbitRuntime, id: &str) -> (Value, usize) {
    let operator = session(McpCapability::Operator);
    let shown = call(runtime, "orbit.notice.show", json!({"id": id}), &operator).unwrap();
    let open = call(runtime, "orbit.notice.list", json!({}), &operator).unwrap();
    (shown, open["notices"].as_array().map_or(0, Vec::len))
}

pub(super) fn managed_notice_mutations_follow_the_host_attested_activity() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let first = add_task(&runtime, "First bundle member");
    let selected = add_task(&runtime, "Selected bundle member");
    let outsider = add_task(&runtime, "Task outside the bundle");
    let run = insert_run(
        &runtime,
        "task_pr_pipeline",
        Some(json!({"task_ids": [first, selected]})),
    );
    let other_run = insert_run(
        &runtime,
        "task_pr_pipeline",
        Some(json!({"task_ids": [outsider]})),
    );
    let post = json!({
        "kind": "incident",
        "title": "Bundle fixture broken",
        "dedupe_key": "bundle-fixture",
        "linked_task": selected
    });

    // The attested leaf task, not the bundle's first member, is the poster.
    let posted = call_with_activity(
        &runtime,
        "orbit.notice.post",
        post.clone(),
        activity(&run, Some(&selected)),
    )
    .expect("the attested activity task posts as an agent");
    assert_eq!(posted["outcome"], "created");
    assert_eq!(posted["notice"]["author"]["class"], "agent");
    assert_eq!(posted["notice"]["author"]["task_id"], selected);
    assert_eq!(posted["notice"]["author"]["run_id"], run);
    let id = posted["notice"]["id"].as_str().unwrap().to_string();
    let before = notice_state(&runtime, &id);

    // Missing, ambiguous or unheld activity authority refuses every mutation,
    // including a corroborating re-post of the same key, and changes nothing.
    for (label, binding) in [
        ("no activity binding", None),
        ("bundle without a single task", activity(&run, None)),
        (
            "task the run does not carry",
            activity(&run, Some(&outsider)),
        ),
        ("blank attested task", activity(&run, Some("  "))),
    ] {
        for (tool, input) in [
            ("orbit.notice.post", post.clone()),
            ("orbit.notice.close", json!({"id": id, "reason": "cleared"})),
            (
                "orbit.notice.close",
                json!({"dedupe_key": "bundle-fixture", "reason": "cleared"}),
            ),
        ] {
            let error = call_with_activity(&runtime, tool, input, binding.clone())
                .expect_err(&format!("{label}: {tool} must refuse"));
            assert!(
                error.starts_with("notice_poster_unidentified"),
                "{label}: {tool} refused with {error}"
            );
            assert_eq!(notice_state(&runtime, &id), before, "{label}: {tool}");
        }
    }

    // Even matching metadata for a real run and task cannot post, corroborate
    // or close through the nested CLI or MCP adapter.
    let matching = managed_pipeline_env(&run, Some(&selected));
    let conflicting = Env::from_vars([
        ("ORBIT_MANAGED_RUN_CONTEXT", "1"),
        ("ORBIT_RUN_ID", run.as_str()),
        ("ORBIT_TASK_ID", first.as_str()),
        ("ORBIT_ACTIVE_TASK_ID", selected.as_str()),
    ]);
    for env in [&matching, &conflicting] {
        for (entry, caller) in [
            (ToolEntryPoint::Cli, local_cli_session(&runtime)),
            (ToolEntryPoint::Mcp, session(McpCapability::Agent)),
        ] {
            for (tool, input) in [
                ("orbit.notice.post", post.clone()),
                ("orbit.notice.close", json!({"id": id, "reason": "cleared"})),
                (
                    "orbit.notice.close",
                    json!({"dedupe_key": "bundle-fixture", "reason": "cleared"}),
                ),
            ] {
                refused(
                    call_with_env(&runtime, env, tool, input, entry, caller.clone()),
                    "notice_poster_unidentified",
                );
                assert_eq!(notice_state(&runtime, &id), before, "{entry:?} {tool}");
            }
        }
    }

    // Another member's attested activity is a valid poster, but not the
    // selected task's notice editor; another run's task is no editor either.
    refused(
        call_with_activity(
            &runtime,
            "orbit.notice.close",
            json!({"id": id, "reason": "cleared"}),
            activity(&run, Some(&first)),
        ),
        "notice_close_refused",
    );
    refused(
        call_with_activity(
            &runtime,
            "orbit.notice.close",
            json!({"id": id, "reason": "cleared"}),
            activity(&other_run, Some(&outsider)),
        ),
        "notice_close_refused",
    );
    assert_eq!(notice_state(&runtime, &id), before, "non-author close");

    // The attested activity keeps its own close path.
    let closed = call_with_activity(
        &runtime,
        "orbit.notice.close",
        json!({"id": id, "reason": "cleared"}),
        activity(&run, Some(&selected)),
    )
    .expect("the attested activity closes its own notice");
    assert_eq!(closed["notice"]["status"], "closed");
    assert_eq!(closed["notice"]["closed"]["by"]["task_id"], selected);
    assert_eq!(closed["notice"]["closed"]["by"]["run_id"], run);
}
