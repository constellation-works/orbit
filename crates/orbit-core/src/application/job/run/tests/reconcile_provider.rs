//! Provider-descendant guards for stale run reconciliation.

use super::*;

use crate::application::job::TERMINAL_OUTCOME_CONFLICT_CODE;
use chrono::{Duration, Utc};
use orbit_common::process::identity::ProcessLiveness;
use orbit_store::V2AuditEventInsertParams;
use orbit_types::workflow::JobRunState;

#[test]
fn live_provider_protects_stale_run_until_normal_completion() {
    let (_root, runtime, run) = stale_run("qa_live_provider");
    reserve_for_run(&runtime, &run.run_id, "file:src/live-provider.rs");
    write_provider_spawn(
        &runtime,
        &run.run_id,
        "provider-live",
        4242,
        "ps-lstart-utc-v1:token-live",
    );

    let reconciled = reconcile_with(&runtime, &run, |pid, token| {
        assert_eq!((pid, token), (4242, Some("ps-lstart-utc-v1:token-live")));
        ProcessLiveness::Alive
    });

    assert!(!reconciled);
    assert_run_state(&runtime, &run, JobRunState::Running);
    assert!(reservation_is_active(&runtime, &run.run_id));

    write_provider_finish(&runtime, &run.run_id, "provider-live");
    assert!(
        runtime
            .stores()
            .jobs()
            .finalize_job_run(&run.run_id, JobRunState::Success, Utc::now(), Some(3_000))
            .expect("provider completes run normally")
    );
    let completed = runtime.show_job_run(&run.run_id).expect("show completion");
    assert_eq!(completed.state, JobRunState::Success);
    assert!(
        completed
            .steps
            .iter()
            .all(|step| { step.error_code.as_deref() != Some(TERMINAL_OUTCOME_CONFLICT_CODE) })
    );
}

#[test]
fn dead_and_reused_provider_pids_do_not_protect_stale_runs() {
    for (job_id, token) in [
        ("qa_dead_provider", "token-dead"),
        ("qa_reused_provider_pid", "token-original-process"),
    ] {
        let (_root, runtime, run) = stale_run(job_id);
        write_provider_spawn(&runtime, &run.run_id, "provider-dead", 5252, token);

        let reconciled = reconcile_with(&runtime, &run, |pid, observed_token| {
            assert_eq!((pid, observed_token), (5252, Some(token)));
            ProcessLiveness::Exited
        });

        assert!(reconciled);
        assert_run_state(&runtime, &run, JobRunState::Interrupted);
    }
}

#[test]
fn unknown_provider_liveness_defers_and_retains_reservation() {
    let (_root, runtime, run) = stale_run("qa_unknown_provider");
    reserve_for_run(&runtime, &run.run_id, "file:src/unknown-provider.rs");
    write_provider_spawn(
        &runtime,
        &run.run_id,
        "provider-unknown",
        6262,
        "token-unknown",
    );

    let reconciled = reconcile_with(&runtime, &run, |_, _| ProcessLiveness::Unknown);

    assert!(!reconciled);
    assert_run_state(&runtime, &run, JobRunState::Running);
    assert!(reservation_is_active(&runtime, &run.run_id));
}

#[test]
fn provider_spawn_during_write_boundary_revalidation_prevents_finalization() {
    let (_root, runtime, run) = stale_run("qa_provider_spawn_race");
    reserve_for_run(&runtime, &run.run_id, "file:src/provider-spawn-race.rs");
    let stale = stored_run(&runtime, &run);

    let reconciled = runtime
        .reconcile_stale_job_run_with_provider_probe_after_classification(
            &stale,
            |pid, token| {
                assert_eq!((pid, token), (7373, Some("ps-lstart-utc-v1:token-race")));
                ProcessLiveness::Alive
            },
            || {
                write_provider_spawn(
                    &runtime,
                    &run.run_id,
                    "provider-race",
                    7373,
                    "ps-lstart-utc-v1:token-race",
                );
            },
        )
        .expect("reconcile provider spawn race");

    assert!(!reconciled);
    assert_run_state(&runtime, &run, JobRunState::Running);
    assert!(reservation_is_active(&runtime, &run.run_id));
}

#[test]
fn old_live_provider_beyond_display_limit_and_audit_page_protects_run() {
    let (_root, runtime, run) = stale_run("qa_old_live_provider");
    write_provider_spawn(
        &runtime,
        &run.run_id,
        "provider-old-live",
        8484,
        "ps-lstart-utc-v1:token-old",
    );
    for index in 0..8 {
        let event_id = format!("provider-newer-{index}");
        write_provider_spawn(
            &runtime,
            &run.run_id,
            &event_id,
            9000 + index,
            "token-finished",
        );
        write_provider_finish(&runtime, &run.run_id, &event_id);
    }

    let reconciled = reconcile_with(&runtime, &run, |pid, token| {
        assert_eq!((pid, token), (8484, Some("ps-lstart-utc-v1:token-old")));
        ProcessLiveness::Alive
    });

    assert!(!reconciled);
    assert_run_state(&runtime, &run, JobRunState::Running);
}

fn stale_run(job_id: &str) -> (tempfile::TempDir, OrbitRuntime, JobRun) {
    let (root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, job_id);
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now() - Duration::seconds(3), 999_999)
        .expect("mark stale running");
    (root, runtime, run)
}

fn reconcile_with<P>(runtime: &OrbitRuntime, run: &JobRun, probe: P) -> bool
where
    P: Fn(u32, Option<&str>) -> ProcessLiveness,
{
    runtime
        .reconcile_stale_job_run_with_provider_probe_after_classification(
            &stored_run(runtime, run),
            probe,
            || {},
        )
        .expect("reconcile stale run")
}

fn stored_run(runtime: &OrbitRuntime, run: &JobRun) -> JobRun {
    runtime
        .get_job_run_backend(&run.run_id)
        .expect("read run")
        .expect("run exists")
}

fn assert_run_state(runtime: &OrbitRuntime, run: &JobRun, expected: JobRunState) {
    assert_eq!(stored_run(runtime, run).state, expected);
}

fn reservation_is_active(runtime: &OrbitRuntime, run_id: &str) -> bool {
    active_reservation_owners(runtime)
        .iter()
        .any(|owner| owner == run_id)
}

fn write_provider_spawn(
    runtime: &OrbitRuntime,
    run_id: &str,
    event_id: &str,
    pid: u32,
    pid_start_time: &str,
) {
    write_provider_event(
        runtime,
        run_id,
        event_id,
        serde_json::json!({
            "body_kind": "cli_invocation_process",
            "pid": pid,
            "pid_start_time": pid_start_time,
        }),
    );
}

fn write_provider_finish(runtime: &OrbitRuntime, run_id: &str, process_event_id: &str) {
    write_provider_event(
        runtime,
        run_id,
        &format!("{process_event_id}-finished"),
        serde_json::json!({
            "body_kind": "cli_invocation_finished",
            "exit_code": 0,
            "timed_out": false,
        }),
    );
}

fn write_provider_event(
    runtime: &OrbitRuntime,
    run_id: &str,
    event_id: &str,
    fields: serde_json::Value,
) {
    let process_event_id = event_id.strip_suffix("-finished").unwrap_or(event_id);
    let parent_event_id = format!("{process_event_id}-invocation");
    let mut event = serde_json::json!({
        "event_id": event_id,
        "ts": Utc::now().to_rfc3339(),
        "run_id": run_id,
        "parent_event_id": parent_event_id,
        "step_id": "agent_implement",
        "provider": "codex",
    });
    event
        .as_object_mut()
        .expect("provider event object")
        .extend(fields.as_object().expect("provider fields object").clone());
    runtime
        .insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: runtime.workspace_id().expect("workspace id"),
            event_id: event_id.to_string(),
            source: "v2_envelope".to_string(),
            schema_version: 1,
            event_type: "activity.progress".to_string(),
            ts: Utc::now(),
            run_id: run_id.to_string(),
            agent_identity: "test".to_string(),
            parent_event_id: Some(parent_event_id),
            workspace_path: None,
            payload_json: event.to_string(),
        })
        .expect("write provider audit event");
}
