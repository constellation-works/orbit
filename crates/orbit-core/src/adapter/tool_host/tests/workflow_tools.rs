use std::collections::BTreeSet;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_tools::ToolContext;
use orbit_types::policy::Role;
use orbit_types::task::TaskStatus;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use orbit_types::workflow::JobRunState;
use serde_json::{Value, json};

use super::super::build_orbit_tool_host;
use super::super::test_support::{
    create_task, managed_tool_env_guard, run_tool_as_operator, test_runtime,
    unmanaged_tool_env_guard,
};
use crate::OrbitRuntime;

/// The default-named ship job. Loaded from the *global* orbit root, so a
/// fixture has to seed it there rather than in the workspace's `.orbit`.
const SHIP_JOB: &str = "task_auto_pipeline";

/// Task ids used by the ship fixtures. Deliberately synthetic: no shipped test
/// fixture may name a real task, path, or workspace.
const SHIP_TASK_IDS: [&str; 2] = ["TST-00001", "TST-00002"];

/// Seed the stub sleep workflow under `<global root>/resources/jobs` so a ship
/// submission resolves a real, enabled job asset without dragging in git or
/// agent machinery.
fn write_ship_job_asset(runtime: &OrbitRuntime) {
    let jobs_dir = runtime.global_root().join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
    std::fs::write(
        jobs_dir.join(format!("{SHIP_JOB}.yaml")),
        format!(
            r#"schemaVersion: 2
kind: Job
metadata:
  name: {SHIP_JOB}
spec:
  state: enabled
  kind: workflow
  steps:
    - id: nap
      spec:
        type: deterministic
        action: sleep
        config: {{}}
"#
        ),
    )
    .expect("write ship job asset");
}

/// A terminal, resumable source run for the `run.resume` half of each
/// direction. `resume` requires an interrupted/failed/timed-out source.
fn seed_failed_run(runtime: &OrbitRuntime) -> String {
    let now = Utc::now();
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(SHIP_JOB, 1, now, Some(json!({"mode": "pr"})), None)
        .expect("insert source run");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, now, std::process::id())
        .expect("start source run");
    runtime
        .stores()
        .jobs()
        .finalize_job_run(&run.run_id, JobRunState::Failed, now, Some(0))
        .expect("finalize source run");
    run.run_id
}

fn ship_input(task_ids: &[String]) -> Value {
    json!({"task_ids": task_ids, "mode": "pr"})
}

fn synthetic_ship_task_ids() -> Vec<String> {
    SHIP_TASK_IDS.iter().map(|id| (*id).to_string()).collect()
}

fn seed_ship_tasks(runtime: &OrbitRuntime, repo_root: &std::path::Path) -> Vec<String> {
    SHIP_TASK_IDS
        .iter()
        .map(|title| {
            create_task(
                runtime,
                repo_root,
                title,
                "synthetic workflow admission fixture",
                TaskStatus::Backlog,
                &[],
            )
            .id
            .to_string()
        })
        .collect()
}

fn capability_denial(result: Result<Value, OrbitError>) -> String {
    match result {
        Err(OrbitError::CapabilityDenied(message)) => message,
        Err(error) => panic!("expected a capability denial, got {error:?}"),
        Ok(value) => panic!("expected a capability denial, got {value}"),
    }
}

/// ORB-10540: the in-run denial, driven by the environment rather than by a
/// hand-built host.
///
/// Nothing here states a run id to the tool layer. `ORBIT_MANAGED_RUN_CONTEXT`
/// authenticates the envelope and `ORBIT_RUN_ID` carries the id;
/// `trusted_env_run_id` turns that pair into the host's task scope during
/// `run_tool_with_context_and_role`, and the scope is what the guard reads.
/// That env-to-scope step is the one the ORB-10534 suite mocked away, and the
/// one GitHub CI cannot exercise because it exports no `ORBIT_*` at all.
///
/// The session still holds operator capability, so the authorization
/// chokepoint admits the call: what refuses it is the self-dispatch guard, not
/// a missing capability.
#[test]
fn managed_run_environment_denies_ship_and_resume_end_to_end() {
    let _env = managed_tool_env_guard("jrun-test-managed");
    let (_root, runtime, _repo_root) = test_runtime();
    write_ship_job_asset(&runtime);
    let source_run_id = seed_failed_run(&runtime);

    // The step the mock skipped: a host built with no explicit run id still
    // reports one, because the environment supplied it.
    assert_eq!(
        build_orbit_tool_host(
            &runtime,
            None,
            None,
            orbit_types::tool::ToolSessionContext::default()
        )
        .task_scope()
        .run_id
        .as_deref(),
        Some("jrun-test-managed"),
    );

    let ship = capability_denial(run_tool_as_operator(
        &runtime,
        "orbit.workflow.ship",
        ship_input(&synthetic_ship_task_ids()),
    ));
    assert!(ship.contains("managed runs cannot dispatch"), "{ship}");

    let resume = capability_denial(run_tool_as_operator(
        &runtime,
        "orbit.workflow.run.resume",
        json!({"id": source_run_id}),
    ));
    assert!(resume.contains("managed runs cannot dispatch"), "{resume}");

    // The denial is the guard's, so nothing was dispatched: the seeded source
    // run is still the only run in the workspace.
    let runs = runtime
        .list_job_runs(crate::application::job::JobRunListParams::default())
        .expect("list runs");
    assert_eq!(
        runs.iter().map(|run| &run.run_id).collect::<Vec<_>>(),
        vec![&source_run_id],
        "a denied dispatch must not persist a run"
    );
}

/// ORB-10540: the permitted direction, same tools and same operator session,
/// with the managed-run envelope absent.
///
/// Without this the denial test above cannot distinguish a working guard from
/// one that refuses unconditionally. Both verbs reach the runtime and produce
/// real runs.
#[test]
fn unmanaged_environment_admits_operator_ship_and_resume() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo_root) = test_runtime();
    write_ship_job_asset(&runtime);
    let source_run_id = seed_failed_run(&runtime);
    let task_ids = seed_ship_tasks(&runtime, &repo_root);

    // The mirror of the denial test's scope assertion: with no envelope there is
    // no run scope, which is what leaves the guard inert.
    assert_eq!(
        build_orbit_tool_host(
            &runtime,
            None,
            None,
            orbit_types::tool::ToolSessionContext::default()
        )
        .task_scope()
        .run_id,
        None,
    );

    let shipped = run_tool_as_operator(&runtime, "orbit.workflow.ship", ship_input(&task_ids))
        .expect("unmanaged operator ship is admitted");
    assert_eq!(shipped["workflow"], json!("ship"));
    assert_eq!(shipped["job_id"], json!(SHIP_JOB));
    let shipped_run_id = shipped["run_id"].as_str().expect("ship run id").to_string();

    let resumed = run_tool_as_operator(
        &runtime,
        "orbit.workflow.run.resume",
        json!({"id": source_run_id}),
    )
    .expect("unmanaged operator resume is admitted");
    assert_eq!(resumed["workflow"], json!("resume"));
    assert_eq!(resumed["retry_source_run_id"], json!(source_run_id));

    let resumed_run_id = resumed["run_id"].as_str().expect("resume run id");
    let stored = runtime
        .show_job_run(&shipped_run_id)
        .expect("shipped run is persisted");
    assert_eq!(stored.job_id, SHIP_JOB);
    assert_eq!(
        stored.input.expect("ship input")["task_ids"],
        json!(task_ids)
    );
    assert_eq!(
        runtime
            .show_job_run(resumed_run_id)
            .expect("resumed run is persisted")
            .retry_source_run_id
            .as_deref(),
        Some(source_run_id.as_str())
    );
}

/// ORB-10544: `orbit.workflow.ship` is a thin projection of the shared
/// submission path, so it inherits that path's duplicate-dispatch guard: a task
/// already carried by a non-terminal run is refused here with the same typed
/// conflict the dashboard endpoint maps to its 409, naming both ids. Before this
/// the check lived only in the endpoint and the tool could dispatch a second run
/// contending for the same worktree and task reservation.
#[test]
fn ship_tool_inherits_the_shared_in_flight_guard() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo_root) = test_runtime();
    write_ship_job_asset(&runtime);
    let task_ids = seed_ship_tasks(&runtime, &repo_root);
    let in_flight = runtime
        .stores()
        .jobs()
        .insert_job_run(
            SHIP_JOB,
            1,
            Utc::now(),
            Some(json!({"mode": "pr", "task_ids": [task_ids[0]]})),
            None,
        )
        .expect("insert in-flight run");

    let error = run_tool_as_operator(&runtime, "orbit.workflow.ship", ship_input(&task_ids))
        .expect_err("the tool must refuse a task already carried by a non-terminal run");

    let OrbitError::ShipRunInFlight { task_id, run_id } = &error else {
        panic!("expected ShipRunInFlight, got {error:?}");
    };
    assert_eq!(task_id, &task_ids[0]);
    assert_eq!(run_id, &in_flight.run_id);

    let runs = runtime
        .list_job_runs(crate::application::job::JobRunListParams::default())
        .expect("list runs");
    assert_eq!(
        runs.iter().map(|run| &run.run_id).collect::<Vec<_>>(),
        vec![&in_flight.run_id],
        "a refused tool dispatch must not persist a run"
    );
}

#[test]
fn ship_tool_parses_and_rejects_an_unknown_crew_allowlist_before_dispatch() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, _repo_root) = test_runtime();

    let error = run_tool_as_operator(
        &runtime,
        "orbit.workflow.ship",
        json!({
            "task_ids": ["TST-00001"],
            "mode": "pr",
            "allowed_crews": ["not-a-configured-crew"],
        }),
    )
    .expect_err("an unknown MCP crew allowlist must fail before dispatch");
    assert!(
        error.to_string().contains("not-a-configured-crew"),
        "{error}"
    );
    assert!(
        runtime
            .list_job_runs(crate::application::job::JobRunListParams::default())
            .expect("list runs")
            .is_empty(),
        "a rejected MCP allowlist must not create a run"
    );
}

/// ORB-10540: the guard is narrow. Inside the same managed envelope that
/// refuses ship and resume, the read-only verbs still answer — a blanket
/// in-run denial would break run observation for every agent.
#[test]
fn managed_run_environment_still_permits_run_observation() {
    let _env = managed_tool_env_guard("jrun-test-managed-observe");
    let (_root, runtime, _repo_root) = test_runtime();
    let source_run_id = seed_failed_run(&runtime);

    let shown = run_tool_as_operator(
        &runtime,
        "orbit.workflow.run.show",
        json!({"id": source_run_id}),
    )
    .expect("run.show remains available inside a managed run");
    assert_eq!(shown["run_id"], json!(source_run_id));

    let listed = run_tool_as_operator(&runtime, "orbit.workflow.run.list", json!({}))
        .expect("run.list remains available inside a managed run");
    assert_eq!(listed["items"][0]["run_id"], json!(source_run_id));
}

#[test]
fn operator_can_observe_runs_and_agent_denial_is_audited() {
    let (_root, runtime, _repo_root) = test_runtime();
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("task_auto_pipeline", 1, Utc::now(), None, None)
        .expect("insert run");

    let shown = run_tool_as_operator(
        &runtime,
        "orbit.workflow.run.show",
        json!({"id": run.run_id}),
    )
    .expect("operator run show");
    assert_eq!(shown["run_id"], json!(run.run_id));

    let listed = run_tool_as_operator(&runtime, "orbit.workflow.run.list", json!({}))
        .expect("operator run list");
    assert_eq!(listed["items"][0]["run_id"], json!(run.run_id));

    let denied = runtime.run_tool_with_context_and_role(
        "orbit.workflow.run.show",
        json!({"id": run.run_id}),
        Role::Admin,
        ToolContext {
            session_context: ToolSessionContext {
                effective_capabilities: BTreeSet::from([McpCapability::Agent]),
                ..ToolSessionContext::default()
            },
            ..ToolContext::default()
        },
    );
    assert!(
        matches!(denied, Err(orbit_common::OrbitError::CapabilityDenied(_))),
        "{denied:?}"
    );

    let audit = runtime
        .list_audit_events(None, None, Some(AuditEventStatus::Denied), None, 20)
        .expect("read denial audit");
    assert!(audit.iter().any(|event| {
        event.command == "authorization"
            && event.target_id.as_deref() == Some("orbit.workflow.run.show")
    }));
}

/// [ORB-10971] CLI, MCP, dashboard API, and audit must agree on lineage. This
/// covers the MCP half: `run.show` and `run.list` project the same durable
/// dispatch checkpoint the other readers do, for a parent that is still
/// blocked on its child.
#[test]
fn mcp_run_observation_names_the_child_a_blocked_parent_dispatched() {
    let (_root, runtime, _repo_root) = test_runtime();
    let parent = runtime
        .stores()
        .jobs()
        .insert_job_run("workspace_auto_pipeline", 1, Utc::now(), None, None)
        .expect("insert parent run");

    let mut state = orbit_types::workflow::PipelineState::new(
        parent.run_id.clone(),
        "workspace_auto_pipeline".to_string(),
        json!({}),
    );
    state.record_child_dispatch(
        orbit_types::workflow::ChildDispatch::submitted(
            "jrun-child-leaves".to_string(),
            "task_auto_pipeline".to_string(),
            "invoke_and_wait".to_string(),
            true,
            false,
            Utc::now(),
        )
        .with_parent_step_id(Some("ship_leaves".to_string())),
    );
    state.advance_child_dispatch(
        "jrun-child-leaves",
        orbit_types::workflow::ChildDispatchPhase::Waiting,
        None,
        None,
    );
    runtime
        .write_run_state(&parent.run_id, &state)
        .expect("seed parent dispatch state");

    let shown = run_tool_as_operator(
        &runtime,
        "orbit.workflow.run.show",
        json!({"id": parent.run_id}),
    )
    .expect("operator run show");
    assert_eq!(
        shown["child_dispatches"][0]["child_run_id"],
        json!("jrun-child-leaves")
    );
    assert_eq!(
        shown["child_dispatches"][0]["job_name"],
        json!("task_auto_pipeline")
    );
    assert_eq!(
        shown["child_dispatches"][0]["parent_step_id"],
        json!("ship_leaves")
    );
    assert_eq!(shown["child_dispatches"][0]["phase"], json!("waiting"));

    let listed = run_tool_as_operator(&runtime, "orbit.workflow.run.list", json!({}))
        .expect("operator run list");
    assert_eq!(
        listed["items"][0]["child_dispatches"][0]["child_run_id"],
        json!("jrun-child-leaves")
    );
}

#[test]
fn mcp_run_observation_carries_an_empty_lineage_for_a_run_without_children() {
    let (_root, runtime, _repo_root) = test_runtime();
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("task_auto_pipeline", 1, Utc::now(), None, None)
        .expect("insert run");

    let shown = run_tool_as_operator(
        &runtime,
        "orbit.workflow.run.show",
        json!({"id": run.run_id}),
    )
    .expect("operator run show");

    assert_eq!(shown["child_dispatches"], json!([]));
}
