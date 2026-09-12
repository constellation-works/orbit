use std::collections::BTreeSet;

use crate::application::job::{AGENT_INVOKE_JOB_ID, JobRunListParams};
use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::process::identity::{
    STABLE_TOKEN_PREFIX, current_pid_namespace, process_start_identity_token,
};
use orbit_tools::ToolContext;
use orbit_types::policy::Role;
use orbit_types::task::TaskStatus;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use orbit_types::workflow::{ChildDispatch, JobRunState, PipelineState};
use serde_json::{Value, json};

use super::super::build_orbit_tool_host;
use super::super::test_support::{
    create_task, managed_tool_env_guard, run_tool_as_operator, test_runtime,
    unmanaged_tool_env_guard,
};
use crate::{OrbitRuntime, V2AuditEventInsertParams};

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

fn seed_recovery_attempt(
    runtime: &OrbitRuntime,
    run_id: &str,
    event_id: &str,
    recovery_succeeded: bool,
    failure_phase: Option<&str>,
    error_message: Option<&str>,
) {
    let event = json!({
        "schemaVersion": 1,
        "event_type": "step.recovery_attempted",
        "event_id": event_id,
        "ts": "2026-09-07T05:25:00Z",
        "run_id": run_id,
        "agent_identity": "codex",
        "body_kind": "step_recovery_attempted",
        "step_id": "implement_one",
        "recovery_activity": "step_failure_recovery",
        "recovery_succeeded": recovery_succeeded,
        "failure_phase": failure_phase,
        "error_message": error_message,
    });
    runtime
        .insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: runtime.workspace_id().expect("workspace id"),
            event_id: event_id.to_string(),
            source: "v2_envelope".to_string(),
            schema_version: 1,
            event_type: "step.recovery_attempted".to_string(),
            ts: chrono::DateTime::parse_from_rfc3339("2026-09-07T05:25:00Z")
                .expect("fixture timestamp")
                .with_timezone(&Utc),
            run_id: run_id.to_string(),
            agent_identity: "codex".to_string(),
            parent_event_id: None,
            workspace_path: None,
            payload_json: event.to_string(),
        })
        .expect("seed recovery attempt audit event");
}

/// Seed one v2 audit event for `run_id`. `body` supplies the event-specific
/// fields; the envelope keys every reader needs are filled in here.
fn seed_v2_event(
    runtime: &OrbitRuntime,
    run_id: &str,
    event_id: &str,
    ts: &str,
    parent_event_id: Option<&str>,
    body: Value,
) {
    let mut event = json!({
        "schemaVersion": 1,
        "event_type": "test.event",
        "event_id": event_id,
        "ts": ts,
        "run_id": run_id,
        "agent_identity": "codex",
        "parent_event_id": parent_event_id,
    });
    let object = event.as_object_mut().expect("event object");
    for (key, value) in body.as_object().expect("event body").clone() {
        object.insert(key, value);
    }
    runtime
        .insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: runtime.workspace_id().expect("workspace id"),
            event_id: event_id.to_string(),
            source: "v2_envelope".to_string(),
            schema_version: 1,
            event_type: "test.event".to_string(),
            ts: chrono::DateTime::parse_from_rfc3339(ts)
                .expect("fixture timestamp")
                .with_timezone(&Utc),
            run_id: run_id.to_string(),
            agent_identity: "codex".to_string(),
            parent_event_id: parent_event_id.map(str::to_string),
            workspace_path: None,
            payload_json: event.to_string(),
        })
        .expect("seed v2 audit event");
}

fn seed_running_run(runtime: &OrbitRuntime) -> String {
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("task_auto_pipeline", 1, Utc::now(), None, None)
        .expect("insert run");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("start run");
    run.run_id
}

/// A PID this process can vouch for, with the identity token that proves the
/// probe is looking at the same process. Deterministically alive: it is the
/// test itself.
fn live_pid_and_token() -> (u32, Option<String>) {
    let pid = std::process::id();
    (pid, process_start_identity_token(pid))
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

#[test]
fn run_list_refuses_a_limit_above_200() {
    let (_root, runtime, _repo_root) = test_runtime();

    let error = run_tool_as_operator(&runtime, "orbit.workflow.run.list", json!({ "limit": 500 }))
        .expect_err("run.list must reject an oversized limit");

    let OrbitError::InvalidInput(message) = error else {
        panic!("expected invalid input");
    };
    assert!(message.contains("200"), "{message}");
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

#[test]
fn mcp_run_show_projects_bounded_recovery_evidence_without_replacing_run_error() {
    let (_root, runtime, _repo_root) = test_runtime();
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("task_auto_pipeline", 1, Utc::now(), None, None)
        .expect("insert run");
    let secret = "sk-proj-abcdefghijklmnopqrstuvwxyz0123456789";
    seed_recovery_attempt(
        &runtime,
        &run.run_id,
        "evt-recovery-failed",
        false,
        Some("dispatch"),
        Some(&format!("recovery launcher refused {secret}")),
    );
    seed_recovery_attempt(
        &runtime,
        &run.run_id,
        "evt-recovery-success",
        true,
        None,
        None,
    );

    let shown = run_tool_as_operator(
        &runtime,
        "orbit.workflow.run.show",
        json!({"id": run.run_id}),
    )
    .expect("operator run show");

    assert_eq!(shown["run_id"], json!(run.run_id));
    assert_eq!(shown["error_message"], Value::Null);
    assert_eq!(shown["recovery_attempts"]["state"], json!("recorded"));
    assert_eq!(shown["recovery_attempts"]["limit"], json!(8));
    assert_eq!(shown["recovery_attempts"]["truncated"], json!(false));
    assert_eq!(
        shown["recovery_attempts"]["items"][0]["run_id"],
        json!(run.run_id)
    );
    assert_eq!(
        shown["recovery_attempts"]["items"][0]["failed_step_id"],
        json!("implement_one")
    );
    assert_eq!(
        shown["recovery_attempts"]["items"][0]["failure_phase"],
        json!("dispatch")
    );
    assert!(
        !shown["recovery_attempts"]["items"][0]["diagnostic"]
            .as_str()
            .unwrap_or_default()
            .contains(secret)
    );
    assert_eq!(
        shown["recovery_attempts"]["items"][1]["outcome"],
        json!("succeeded")
    );
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

/// [ORB-12255] MCP run show/list reconstruct steps from the audit trail when
/// the run record stores none, and name the source.
#[test]
fn mcp_run_show_and_list_reconstruct_empty_record_steps() {
    let (_root, runtime, _repo_root) = test_runtime();
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("task_auto_pipeline", 1, Utc::now(), None, None)
        .expect("insert run");
    runtime
        .insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: runtime.workspace_id().expect("workspace id"),
            event_id: "evt-step-start".to_string(),
            source: "v2_envelope".to_string(),
            schema_version: 1,
            event_type: "step.started".to_string(),
            ts: chrono::DateTime::parse_from_rfc3339("2026-09-12T04:00:00Z")
                .expect("fixture timestamp")
                .with_timezone(&Utc),
            run_id: run.run_id.clone(),
            agent_identity: "codex".to_string(),
            parent_event_id: None,
            workspace_path: None,
            payload_json: json!({
                "schemaVersion": 1,
                "event_type": "step.started",
                "event_id": "evt-step-start",
                "ts": "2026-09-12T04:00:00Z",
                "run_id": run.run_id,
                "agent_identity": "codex",
                "body_kind": "step_started",
                "step_id": "nap",
            })
            .to_string(),
        })
        .expect("insert step.started");
    runtime
        .insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: runtime.workspace_id().expect("workspace id"),
            event_id: "evt-step-finish".to_string(),
            source: "v2_envelope".to_string(),
            schema_version: 1,
            event_type: "step.finished".to_string(),
            ts: chrono::DateTime::parse_from_rfc3339("2026-09-12T04:00:01Z")
                .expect("fixture timestamp")
                .with_timezone(&Utc),
            run_id: run.run_id.clone(),
            agent_identity: "codex".to_string(),
            parent_event_id: None,
            workspace_path: None,
            payload_json: json!({
                "schemaVersion": 1,
                "event_type": "step.finished",
                "event_id": "evt-step-finish",
                "ts": "2026-09-12T04:00:01Z",
                "run_id": run.run_id,
                "agent_identity": "codex",
                "body_kind": "step_finished",
                "step_id": "nap",
                "outcome": "success",
            })
            .to_string(),
        })
        .expect("insert step.finished");

    let shown = run_tool_as_operator(
        &runtime,
        "orbit.workflow.run.show",
        json!({"id": run.run_id}),
    )
    .expect("operator run show");
    assert_eq!(shown["steps_source"], json!("audit"));
    assert_eq!(shown["steps"][0]["target_id"], json!("nap"));
    assert_eq!(shown["steps"][0]["state"], json!("success"));

    let listed = run_tool_as_operator(&runtime, "orbit.workflow.run.list", json!({}))
        .expect("operator run list");
    assert_eq!(listed["items"][0]["steps_source"], json!("audit"));
    assert_eq!(listed["items"][0]["steps"][0]["target_id"], json!("nap"));
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

/// [ORB-11752] The gap this task closes: a `running` run whose registered
/// projection reported a wrapper PID and nothing about the implementation
/// child. `execution_progress` names the open step and the live provider
/// process, so an orchestrator can tell a working agent from an abandoned
/// wrapper without falling back to an operator command.
#[test]
fn mcp_run_show_names_the_active_step_and_its_live_provider_child() {
    let (_root, runtime, _repo_root) = test_runtime();
    let run_id = seed_running_run(&runtime);
    let (pid, pid_start_time) = live_pid_and_token();

    seed_v2_event(
        &runtime,
        &run_id,
        "evt-prepare",
        "2026-09-08T00:06:00Z",
        None,
        json!({"body_kind": "step_started", "step_id": "prepare_worktree"}),
    );
    seed_v2_event(
        &runtime,
        &run_id,
        "evt-prepare-done",
        "2026-09-08T00:06:10Z",
        None,
        json!({"body_kind": "step_finished", "step_id": "prepare_worktree", "outcome": "success"}),
    );
    seed_v2_event(
        &runtime,
        &run_id,
        "evt-implement",
        "2026-09-08T00:06:20Z",
        None,
        json!({"body_kind": "step_started", "step_id": "implement_one"}),
    );
    seed_v2_event(
        &runtime,
        &run_id,
        "evt-activity",
        "2026-09-08T00:06:21Z",
        Some("evt-implement"),
        json!({"body_kind": "activity_started"}),
    );
    seed_v2_event(
        &runtime,
        &run_id,
        "evt-process",
        "2026-09-08T00:06:22Z",
        Some("evt-activity"),
        json!({
            "body_kind": "cli_invocation_process",
            "provider": "codex",
            "pid": pid,
            "pid_start_time": pid_start_time,
        }),
    );

    let shown = run_tool_as_operator(&runtime, "orbit.workflow.run.show", json!({"id": run_id}))
        .expect("operator run show");

    let progress = &shown["execution_progress"];
    assert_eq!(progress["state"], json!("observed"));
    assert_eq!(progress["active_step"]["step_id"], json!("implement_one"));
    assert_eq!(progress["active_step"]["step_index"], json!(1));
    assert_eq!(
        progress["active_step"]["started_at"],
        json!("2026-09-08T00:06:20+00:00")
    );

    let processes = &progress["provider_processes"];
    assert_eq!(processes["truncated"], json!(false));
    assert_eq!(processes["items"].as_array().expect("items").len(), 1);
    let child = &processes["items"][0];
    assert_eq!(child["pid"], json!(pid));
    assert_eq!(child["provider"], json!("codex"));
    assert_eq!(child["step_id"], json!("implement_one"));
    assert_eq!(child["step_index"], json!(1));
    assert_eq!(child["finished"], json!(false));
    assert_eq!(child["liveness"], json!("alive"));

    // The evidence is additive: everything the surface already answered is
    // still on the same response.
    assert_eq!(shown["run_id"], json!(run_id));
    assert_eq!(shown["state"], json!("running"));
    assert_eq!(shown["child_dispatches"], json!([]));
    assert_eq!(shown["recovery_attempts"]["state"], json!("not_attempted"));
    assert_eq!(shown["agent_invocation"], Value::Null);

    // `list` pages up to 200 runs, so it does not pay for an audit scan and a
    // liveness probe per row.
    let listed = run_tool_as_operator(&runtime, "orbit.workflow.run.list", json!({}))
        .expect("operator run list");
    assert_eq!(listed["items"][0]["execution_progress"], Value::Null);
}

/// Retries and parallel invocations under one step stay separable: each spawn
/// keeps its own exit evidence, and only the child that never reported an exit
/// is probed for liveness.
#[test]
fn mcp_run_show_separates_a_finished_child_from_the_open_retry() {
    let (_root, runtime, _repo_root) = test_runtime();
    let run_id = seed_running_run(&runtime);
    let (pid, pid_start_time) = live_pid_and_token();

    seed_v2_event(
        &runtime,
        &run_id,
        "evt-step",
        "2026-09-08T00:06:00Z",
        None,
        json!({"body_kind": "step_started", "step_id": "implement_one"}),
    );
    for (activity, process, spawned_pid) in [
        ("evt-first", "evt-first-pid", 424_242_u32),
        ("evt-second", "evt-second-pid", pid),
    ] {
        seed_v2_event(
            &runtime,
            &run_id,
            activity,
            "2026-09-08T00:06:01Z",
            Some("evt-step"),
            json!({"body_kind": "activity_started"}),
        );
        seed_v2_event(
            &runtime,
            &run_id,
            process,
            "2026-09-08T00:06:02Z",
            Some(activity),
            json!({
                "body_kind": "cli_invocation_process",
                "provider": "codex",
                "pid": spawned_pid,
                "pid_start_time": if spawned_pid == pid { json!(pid_start_time) } else { Value::Null },
            }),
        );
    }
    seed_v2_event(
        &runtime,
        &run_id,
        "evt-first-done",
        "2026-09-08T00:06:30Z",
        Some("evt-first"),
        json!({"body_kind": "cli_invocation_finished", "exit_code": 137, "duration_ms": 28_000}),
    );

    let shown = run_tool_as_operator(&runtime, "orbit.workflow.run.show", json!({"id": run_id}))
        .expect("operator run show");
    let items = shown["execution_progress"]["provider_processes"]["items"]
        .as_array()
        .expect("items")
        .clone();

    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["pid"], json!(424_242));
    assert_eq!(items[0]["finished"], json!(true));
    assert_eq!(items[0]["exit_code"], json!(137));
    assert_eq!(items[0]["duration_ms"], json!(28_000));
    // A child that reported its exit is not probed: it is `exited` by record.
    assert_eq!(items[0]["liveness"], json!("exited"));

    assert_eq!(items[1]["pid"], json!(pid));
    assert_eq!(items[1]["finished"], json!(false));
    assert_eq!(items[1]["exit_code"], Value::Null);
    assert_eq!(items[1]["liveness"], json!("alive"));
}

/// A recycled PID is not a live agent. The recorded identity token is what
/// separates "my child is still running" from "some unrelated process now
/// holds that number", and a PID recorded in a foreign PID namespace is
/// `unknown` rather than a false `exited`.
#[test]
fn mcp_run_show_rejects_a_recycled_pid_and_refuses_to_judge_a_foreign_namespace() {
    let (_root, runtime, _repo_root) = test_runtime();
    let run_id = seed_running_run(&runtime);
    let pid = std::process::id();

    seed_v2_event(
        &runtime,
        &run_id,
        "evt-step",
        "2026-09-08T00:06:00Z",
        None,
        json!({"body_kind": "step_started", "step_id": "implement_one"}),
    );
    seed_v2_event(
        &runtime,
        &run_id,
        "evt-recycled",
        "2026-09-08T00:06:02Z",
        Some("evt-step"),
        json!({
            "body_kind": "cli_invocation_process",
            "provider": "codex",
            "pid": pid,
            // A live PID whose recorded start identity disagrees with the
            // process holding it now.
            "pid_start_time": format!(
                "{}pidns={}:Thu Jan  1 00:00:00 1970",
                STABLE_TOKEN_PREFIX,
                current_pid_namespace().unwrap_or("-"),
            ),
        }),
    );

    let shown = run_tool_as_operator(&runtime, "orbit.workflow.run.show", json!({"id": run_id}))
        .expect("operator run show");
    assert_eq!(
        shown["execution_progress"]["provider_processes"]["items"][0]["liveness"],
        json!("exited")
    );

    // Only Linux reports a PID namespace, so only there can a reader stand
    // outside the one a PID was recorded in.
    #[cfg(target_os = "linux")]
    {
        let foreign_run_id = seed_running_run(&runtime);
        seed_v2_event(
            &runtime,
            &foreign_run_id,
            "evt-foreign-step",
            "2026-09-08T00:06:00Z",
            None,
            json!({"body_kind": "step_started", "step_id": "implement_one"}),
        );
        seed_v2_event(
            &runtime,
            &foreign_run_id,
            "evt-foreign",
            "2026-09-08T00:06:02Z",
            Some("evt-foreign-step"),
            json!({
                "body_kind": "cli_invocation_process",
                "provider": "codex",
                "pid": pid,
                "pid_start_time": format!("{STABLE_TOKEN_PREFIX}pidns=1:Thu Jan  1 00:00:00 1970"),
            }),
        );

        let foreign = run_tool_as_operator(
            &runtime,
            "orbit.workflow.run.show",
            json!({"id": foreign_run_id}),
        )
        .expect("operator run show");
        assert_eq!(
            foreign["execution_progress"]["provider_processes"]["items"][0]["liveness"],
            json!("unknown")
        );
    }
}

/// A run with no v2 audit trail — a legacy run, or one whose trail was never
/// written — reports `unavailable`, which is not the same claim as "nothing is
/// running". A long retry history is bounded, and the open child keeps its
/// place in the budget.
#[test]
fn mcp_run_show_marks_a_missing_trail_unavailable_and_bounds_a_long_history() {
    let (_root, runtime, _repo_root) = test_runtime();
    let legacy_run_id = seed_running_run(&runtime);

    let legacy = run_tool_as_operator(
        &runtime,
        "orbit.workflow.run.show",
        json!({"id": legacy_run_id}),
    )
    .expect("operator run show");
    let progress = &legacy["execution_progress"];
    assert_eq!(progress["state"], json!("unavailable"));
    assert_eq!(progress["active_step"], Value::Null);
    assert_eq!(progress["provider_processes"]["items"], json!([]));
    assert_eq!(progress["provider_processes"]["limit"], json!(8));
    assert_eq!(progress["provider_processes"]["truncated"], json!(false));

    let busy_run_id = seed_running_run(&runtime);
    seed_v2_event(
        &runtime,
        &busy_run_id,
        "evt-step",
        "2026-09-08T00:06:00Z",
        None,
        json!({"body_kind": "step_started", "step_id": "implement_one"}),
    );
    // One open child spawned first, then more finished retries than the budget
    // carries: the open one must survive the truncation.
    let (pid, pid_start_time) = live_pid_and_token();
    seed_v2_event(
        &runtime,
        &busy_run_id,
        "evt-open-activity",
        "2026-09-08T00:06:01Z",
        Some("evt-step"),
        json!({"body_kind": "activity_started"}),
    );
    seed_v2_event(
        &runtime,
        &busy_run_id,
        "evt-open-pid",
        "2026-09-08T00:06:02Z",
        Some("evt-open-activity"),
        json!({
            "body_kind": "cli_invocation_process",
            "provider": "codex",
            "pid": pid,
            "pid_start_time": pid_start_time,
        }),
    );
    for retry in 0..12u32 {
        let activity = format!("evt-retry-{retry}-activity");
        seed_v2_event(
            &runtime,
            &busy_run_id,
            &activity,
            &format!("2026-09-08T00:07:{retry:02}Z"),
            Some("evt-step"),
            json!({"body_kind": "activity_started"}),
        );
        seed_v2_event(
            &runtime,
            &busy_run_id,
            &format!("evt-retry-{retry}-pid"),
            &format!("2026-09-08T00:08:{retry:02}Z"),
            Some(&activity),
            json!({
                "body_kind": "cli_invocation_process",
                "provider": "codex",
                "pid": 500_000 + retry,
            }),
        );
        seed_v2_event(
            &runtime,
            &busy_run_id,
            &format!("evt-retry-{retry}-done"),
            &format!("2026-09-08T00:09:{retry:02}Z"),
            Some(&activity),
            json!({"body_kind": "cli_invocation_finished", "exit_code": 1}),
        );
    }

    let busy = run_tool_as_operator(
        &runtime,
        "orbit.workflow.run.show",
        json!({"id": busy_run_id}),
    )
    .expect("operator run show");
    let processes = &busy["execution_progress"]["provider_processes"];
    let items = processes["items"].as_array().expect("items");
    assert_eq!(processes["truncated"], json!(true));
    assert_eq!(items.len(), 8);
    assert_eq!(items[0]["pid"], json!(pid), "the open child must survive");
    assert_eq!(items[0]["liveness"], json!("alive"));
    assert_eq!(
        items
            .iter()
            .skip(1)
            .map(|item| item["pid"].as_u64().expect("pid"))
            .collect::<Vec<_>>(),
        (500_005_u64..=500_011).collect::<Vec<_>>(),
        "the newest finished retries fill the rest of the budget"
    );
}

fn listed_item<'a>(listed: &'a Value, run_id: &str) -> &'a Value {
    listed["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["run_id"] == json!(run_id))
        .unwrap_or_else(|| panic!("missing listed run {run_id}"))
}

fn insert_named_run(runtime: &OrbitRuntime, job_id: &str) -> String {
    runtime
        .stores()
        .jobs()
        .insert_job_run(job_id, 1, Utc::now(), None, None)
        .expect("insert run")
        .run_id
}

fn seed_numbered_recovery(runtime: &OrbitRuntime, run_id: &str, count: u32) {
    for index in 0..count {
        seed_v2_event(
            runtime,
            run_id,
            &format!("{run_id}-evt-recovery-{index}"),
            &format!("2026-09-07T05:25:{index:02}Z"),
            None,
            json!({
                "event_type": "step.recovery_attempted",
                "body_kind": "step_recovery_attempted",
                "step_id": "implement_one",
                "recovery_activity": "step_failure_recovery",
                "recovery_succeeded": index % 2 == 1,
            }),
        );
    }
}

/// [ORB-11625] Default list keeps the enriched contract: lineage, drain
/// controls, invocation results, and recovery availability/truncation.
#[test]
fn mcp_run_list_preserves_enriched_default_projection_across_a_mixed_page() {
    let (_root, runtime, _repo_root) = test_runtime();

    let legacy_id = insert_named_run(&runtime, "task_auto_pipeline");

    let not_attempted_id = insert_named_run(&runtime, "task_auto_pipeline");
    seed_v2_event(
        &runtime,
        &not_attempted_id,
        "evt-started",
        "2026-09-07T05:24:00Z",
        None,
        json!({"body_kind": "step_started", "step_id": "implement_one"}),
    );

    let recorded_id = insert_named_run(&runtime, "task_auto_pipeline");
    seed_numbered_recovery(&runtime, &recorded_id, 2);

    let truncated_id = insert_named_run(&runtime, "task_auto_pipeline");
    seed_numbered_recovery(&runtime, &truncated_id, 9);

    let drain_id = insert_named_run(&runtime, "workspace_auto_pipeline");
    let mut drain_state = PipelineState::new(
        drain_id.clone(),
        "workspace_auto_pipeline".to_string(),
        json!({}),
    );
    drain_state.record_child_dispatch(
        ChildDispatch::submitted(
            "jrun-child-leaves".to_string(),
            "task_auto_pipeline".to_string(),
            "invoke_and_wait".to_string(),
            true,
            false,
            Utc::now(),
        )
        .with_parent_step_id(Some("ship_leaves".to_string())),
    );
    assert!(drain_state.set_drain_worker_limit(
        7,
        5,
        "operator".to_string(),
        Some("raise".to_string()),
        None,
    ));
    assert!(
        drain_state
            .set_drain_admissions_stop("operator".to_string(), Some("window closed".to_string()))
    );
    runtime
        .write_run_state(&drain_id, &drain_state)
        .expect("seed drain state");

    let invoke_id = insert_named_run(&runtime, AGENT_INVOKE_JOB_ID);
    let mut invoke_state = PipelineState::new(
        invoke_id.clone(),
        AGENT_INVOKE_JOB_ID.to_string(),
        json!({}),
    );
    invoke_state.step_outputs.insert(
        0,
        json!({
            "exit_code": 0,
            "timed_out": false,
            "completion_envelope_satisfied": true,
            "summary": "the clock restarts",
            "stdout_text": "hello",
            "stdout_blob_ref": "blob-list",
        }),
    );
    runtime
        .write_run_state(&invoke_id, &invoke_state)
        .expect("seed invoke state");

    let listed = run_tool_as_operator(&runtime, "orbit.workflow.run.list", json!({}))
        .expect("operator run list");
    assert_eq!(listed["items"].as_array().expect("items").len(), 6);

    let legacy = listed_item(&listed, &legacy_id);
    assert_eq!(legacy["child_dispatches"], json!([]));
    assert_eq!(legacy["drain_worker_limit"], Value::Null);
    assert_eq!(legacy["drain_admissions_stop"], Value::Null);
    assert_eq!(legacy["agent_invocation"], Value::Null);
    assert_eq!(legacy["recovery_attempts"]["state"], json!("unavailable"));
    assert_eq!(legacy["recovery_attempts"]["limit"], json!(8));
    assert_eq!(legacy["recovery_attempts"]["truncated"], json!(false));
    assert_eq!(legacy["execution_progress"], Value::Null);

    let not_attempted = listed_item(&listed, &not_attempted_id);
    assert_eq!(
        not_attempted["recovery_attempts"]["state"],
        json!("not_attempted")
    );
    assert_eq!(not_attempted["recovery_attempts"]["items"], json!([]));

    let recorded = listed_item(&listed, &recorded_id);
    assert_eq!(recorded["recovery_attempts"]["state"], json!("recorded"));
    assert_eq!(recorded["recovery_attempts"]["truncated"], json!(false));
    assert_eq!(
        recorded["recovery_attempts"]["items"][0]["event_id"],
        json!(format!("{recorded_id}-evt-recovery-0"))
    );
    assert_eq!(
        recorded["recovery_attempts"]["items"][1]["event_id"],
        json!(format!("{recorded_id}-evt-recovery-1"))
    );
    assert_eq!(
        recorded["recovery_attempts"]["items"][0]["run_id"],
        json!(recorded_id)
    );

    let truncated = listed_item(&listed, &truncated_id);
    assert_eq!(truncated["recovery_attempts"]["truncated"], json!(true));
    assert_eq!(
        truncated["recovery_attempts"]["items"]
            .as_array()
            .expect("items")
            .len(),
        8
    );
    assert_eq!(
        truncated["recovery_attempts"]["items"][0]["event_id"],
        json!(format!("{truncated_id}-evt-recovery-1"))
    );
    assert_eq!(
        truncated["recovery_attempts"]["items"][7]["event_id"],
        json!(format!("{truncated_id}-evt-recovery-8"))
    );

    let drain = listed_item(&listed, &drain_id);
    assert_eq!(
        drain["child_dispatches"][0]["child_run_id"],
        json!("jrun-child-leaves")
    );
    assert_eq!(
        drain["drain_worker_limit"]["max_active_leaf_runs"],
        json!(7)
    );
    assert_eq!(drain["drain_admissions_stop"]["actor"], json!("operator"));

    let invoke = listed_item(&listed, &invoke_id);
    assert_eq!(
        invoke["agent_invocation"]["completed_envelope"],
        json!(true)
    );
    assert_eq!(
        invoke["agent_invocation"]["summary"],
        json!("the clock restarts")
    );

    let shown_truncated = run_tool_as_operator(
        &runtime,
        "orbit.workflow.run.show",
        json!({"id": truncated_id}),
    )
    .expect("operator run show");
    assert_eq!(
        shown_truncated["recovery_attempts"],
        truncated["recovery_attempts"]
    );
    assert_eq!(
        shown_truncated["execution_progress"]["state"],
        json!("observed")
    );
}

/// [ORB-11625] List enrichment is one state read and two bounded recovery
/// queries for the page, not one full envelope scan per run. Reconciliation
/// inside `list_job_runs` is excluded from these counts.
#[test]
fn mcp_run_list_projection_batches_state_and_recovery_reads_for_a_page() {
    let (_root, runtime, _repo_root) = test_runtime();
    let quiet = insert_named_run(&runtime, "task_auto_pipeline");
    let busy = insert_named_run(&runtime, "task_auto_pipeline");
    seed_v2_event(
        &runtime,
        &quiet,
        "evt-quiet",
        "2026-09-07T05:24:00Z",
        None,
        json!({"body_kind": "step_started", "step_id": "implement_one"}),
    );
    seed_numbered_recovery(&runtime, &busy, 12);

    let runs = runtime
        .list_job_runs(JobRunListParams::default())
        .expect("load page");
    let (items, reads) = super::super::workflow_tools::project_workflow_run_list(&runtime, &runs)
        .expect("project list");

    assert_eq!(items.len(), 2);
    assert_eq!(reads.pipeline_state_queries, 1);
    assert_eq!(reads.recovery_event_queries, 1);
    assert_eq!(reads.recovery_presence_queries, 1);
    assert_eq!(reads.per_run_recovery_fetch_limit, 9);
    assert_eq!(reads.per_run_recovery_projection_limit, 8);

    let empty_reads = super::super::workflow_tools::project_workflow_run_list(&runtime, &[])
        .expect("empty page")
        .1;
    assert_eq!(empty_reads.pipeline_state_queries, 0);
    assert_eq!(empty_reads.recovery_event_queries, 0);
    assert_eq!(empty_reads.recovery_presence_queries, 0);
    assert_eq!(empty_reads.per_run_recovery_fetch_limit, 9);
}

/// [ORB-11625] A busy earlier run cannot steal a later run's recovery
/// evidence. Missing and unreadable audit stay `unavailable`.
#[test]
fn mcp_run_list_keeps_per_run_recovery_attribution_with_uneven_histories() {
    let (_root, runtime, _repo_root) = test_runtime();
    let missing_id = insert_named_run(&runtime, "task_auto_pipeline");
    let unreadable_id = insert_named_run(&runtime, "task_auto_pipeline");
    runtime
        .insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: runtime.workspace_id().expect("workspace id"),
            event_id: "evt-malformed".to_string(),
            source: "v2_envelope".to_string(),
            schema_version: 1,
            event_type: "test.event".to_string(),
            ts: Utc::now(),
            run_id: unreadable_id.clone(),
            agent_identity: "codex".to_string(),
            parent_event_id: None,
            workspace_path: None,
            payload_json: "{not-json".to_string(),
        })
        .expect("seed unreadable audit");
    let quiet_id = insert_named_run(&runtime, "task_auto_pipeline");
    seed_numbered_recovery(&runtime, &quiet_id, 1);
    let busy_id = insert_named_run(&runtime, "task_auto_pipeline");
    seed_numbered_recovery(&runtime, &busy_id, 20);

    let listed = run_tool_as_operator(&runtime, "orbit.workflow.run.list", json!({}))
        .expect("operator run list");

    let missing = listed_item(&listed, &missing_id);
    assert_eq!(missing["recovery_attempts"]["state"], json!("unavailable"));
    assert_eq!(missing["recovery_attempts"]["items"], json!([]));

    let unreadable = listed_item(&listed, &unreadable_id);
    assert_eq!(
        unreadable["recovery_attempts"]["state"],
        json!("unavailable")
    );

    let quiet = listed_item(&listed, &quiet_id);
    assert_eq!(quiet["recovery_attempts"]["state"], json!("recorded"));
    assert_eq!(quiet["recovery_attempts"]["truncated"], json!(false));
    assert_eq!(
        quiet["recovery_attempts"]["items"][0]["event_id"],
        json!(format!("{quiet_id}-evt-recovery-0"))
    );
    assert_eq!(
        quiet["recovery_attempts"]["items"][0]["run_id"],
        json!(quiet_id)
    );

    let busy = listed_item(&listed, &busy_id);
    assert_eq!(busy["recovery_attempts"]["truncated"], json!(true));
    let busy_ids = busy["recovery_attempts"]["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|item| item["event_id"].as_str().expect("event_id").to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        busy_ids,
        (12..20)
            .map(|index| format!("{busy_id}-evt-recovery-{index}"))
            .collect::<Vec<_>>()
    );
    assert!(
        busy["recovery_attempts"]["items"]
            .as_array()
            .expect("items")
            .iter()
            .all(|item| item["run_id"] == json!(busy_id))
    );
}
