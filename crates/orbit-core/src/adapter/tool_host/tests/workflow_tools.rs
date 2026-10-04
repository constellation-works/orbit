use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::process::identity::{STABLE_TOKEN_PREFIX, current_pid_namespace};
use orbit_types::workflow::JobRunState;
use serde_json::{Value, json};

use super::super::build_orbit_tool_host;
use super::super::test_support::{managed_tool_env_guard, run_tool_as_operator, test_runtime};
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

fn capability_denial(result: Result<Value, OrbitError>) -> String {
    match result {
        Err(OrbitError::CapabilityDenied(message)) => message,
        Err(error) => panic!("expected a capability denial, got {error:?}"),
        Ok(value) => panic!("expected a capability denial, got {value}"),
    }
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
    // Recycled-PID detection re-derives the identity from `ps`. A host that
    // denies executing `ps` (the macOS agent-executor sandbox) cannot answer,
    // and the documented fail-safe keeps a PID that `kill(pid, 0)` still
    // sees `alive`: an unanswerable probe is never evidence of death.
    let expected_liveness = match orbit_common::test_env::start_identity_probe_blocker() {
        None => "exited",
        Some(reason) => {
            tracing::warn!(%reason, "asserting the probe-unavailable fail-safe instead of recycled-PID detection");
            "alive"
        }
    };
    assert_eq!(
        shown["execution_progress"]["provider_processes"]["items"][0]["liveness"],
        json!(expected_liveness)
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

/// [ORB-13899] A running invocation shows what its agent last said and when
/// it last produced output, before any answer exists. A later sample whose
/// output tail held no message keeps the earlier message but moves the time.
#[test]
fn mcp_run_show_reports_a_running_invocations_latest_message() {
    let (_root, runtime, _repo_root) = test_runtime();
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(
            crate::application::job::AGENT_INVOKE_JOB_ID,
            1,
            Utc::now(),
            None,
            None,
        )
        .expect("insert run");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("start run");
    let run_id = run.run_id;

    seed_v2_event(
        &runtime,
        &run_id,
        "evt-step",
        "2026-10-04T08:00:00Z",
        None,
        json!({"body_kind": "step_started", "step_id": "invoke"}),
    );
    seed_v2_event(
        &runtime,
        &run_id,
        "evt-process",
        "2026-10-04T08:00:01Z",
        Some("evt-step"),
        json!({"body_kind": "cli_invocation_process", "provider": "codex", "pid": std::process::id()}),
    );
    seed_v2_event(
        &runtime,
        &run_id,
        "evt-activity-1",
        "2026-10-04T08:00:11Z",
        Some("evt-step"),
        json!({
            "body_kind": "cli_invocation_activity",
            "provider": "codex",
            "observed_bytes": 512,
            "latest_message": "Reading the sweep clock config.",
        }),
    );
    seed_v2_event(
        &runtime,
        &run_id,
        "evt-activity-2",
        "2026-10-04T08:00:21Z",
        Some("evt-step"),
        json!({"body_kind": "cli_invocation_activity", "provider": "codex", "observed_bytes": 2048}),
    );

    let shown = run_tool_as_operator(&runtime, "orbit.workflow.run.show", json!({"id": run_id}))
        .expect("operator run show");
    let invocation = &shown["agent_invocation"];
    assert_eq!(invocation["outcome"], "running", "{shown}");
    assert_eq!(invocation["answer"], Value::Null, "{shown}");
    assert_eq!(
        invocation["progress"],
        json!({
            "last_activity_at": "2026-10-04T08:00:21+00:00",
            "latest_message": "Reading the sweep clock config.",
            "latest_message_truncated": false,
        })
    );
    let child = &shown["execution_progress"]["provider_processes"]["items"][0];
    assert_eq!(child["latest_message"], "Reading the sweep clock config.");
    assert_eq!(child["last_activity_at"], "2026-10-04T08:00:21+00:00");
}
