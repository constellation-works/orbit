//! Deterministic cancellation-versus-worker terminalization interleavings.

use super::*;

use orbit_engine::RuntimeHost;
#[cfg(unix)]
use orbit_store::TaskReservationReserveParams;

fn mark_running(runtime: &OrbitRuntime, run: &JobRun) {
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("mark running");
}

fn has_conflict(runtime: &OrbitRuntime, run_id: &str) -> bool {
    runtime
        .show_job_run(run_id)
        .expect("show run")
        .steps
        .iter()
        .any(|step| {
            step.error_code.as_deref()
                == Some(crate::application::job::TERMINAL_OUTCOME_CONFLICT_CODE)
        })
}

#[cfg(unix)]
fn reserve_for_run(runtime: &OrbitRuntime, owner_run_id: &str, file: &str) {
    runtime
        .stores()
        .task_reservations()
        .reserve_task_reservation(TaskReservationReserveParams {
            workspace_orbit_dir: runtime.paths().orbit_dir.to_string_lossy().into_owned(),
            workspace_id: Some(runtime.workspace_id().expect("workspace id")),
            task_ids: Vec::new(),
            requested_files: vec![file.to_string()],
            actor: "test".to_string(),
            ttl_seconds: 3_600,
            owner_run_id: Some(owner_run_id.to_string()),
            owner_metadata_json: None,
        })
        .expect("reserve for run");
}

#[cfg(unix)]
fn active_reservation_owners(runtime: &OrbitRuntime) -> Vec<String> {
    let workspace_orbit_dir = runtime.paths().orbit_dir.to_string_lossy().into_owned();
    let workspace_id = runtime.workspace_id().expect("workspace id");
    runtime
        .stores()
        .task_reservations()
        .list_active_task_reservations(&workspace_orbit_dir, Some(&workspace_id))
        .expect("list active reservations")
        .reservations
        .into_iter()
        .filter_map(|reservation| reservation.owner_run_id)
        .collect()
}

/// Exact incident interleaving, without wall-clock sleeps: cancellation intent
/// is durable, the worker observer sees SIGTERM, and the signaller has not yet
/// returned from authoritative process-group verification.
#[cfg(unix)]
#[test]
fn signal_induced_worker_exit_defers_to_verified_cancellation() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_cancel_signal_race");
    mark_running(&runtime, &run);
    reserve_for_run(&runtime, &run.run_id, "file:src/cancelled.rs");
    reserve_for_run(&runtime, "jrun-unrelated", "file:src/unrelated.rs");

    let result = runtime
        .cancel_job_run_with_signaller(&run.run_id, "tester", "unit", |owned| {
            assert_eq!(owned.pid, Some(std::process::id()));
            assert!(
                runtime
                    .active_job_run_cancellation_request(&run.run_id)
                    .expect("read cancellation intent")
                    .is_some(),
                "intent must be durable before any signal is sent"
            );
            assert!(
                runtime
                    .record_pipeline_worker_cancellation_exit(
                        owned,
                        libc::SIGTERM,
                        "signal: 15 (SIGTERM)",
                        Some("test-observer"),
                    )
                    .expect("record signal-induced exit")
            );
            assert_eq!(
                runtime.show_job_run(&run.run_id).expect("show run").state,
                JobRunState::Running,
                "reaping only the leader must not release terminal-owned resources"
            );
            let owners = active_reservation_owners(&runtime);
            assert!(owners.iter().any(|owner| owner == &run.run_id));
            assert!(owners.iter().any(|owner| owner == "jrun-unrelated"));
            Ok("terminated_process_group".to_string())
        })
        .expect("cancel after verified termination");

    assert_eq!(result.outcome, "cancelled");
    assert_eq!(result.final_state, "cancelled");
    assert_eq!(
        runtime.show_job_run(&run.run_id).expect("show run").state,
        JobRunState::Cancelled
    );
    assert!(!has_conflict(&runtime, &run.run_id));
    assert_eq!(
        active_reservation_owners(&runtime),
        vec!["jrun-unrelated".to_string()],
        "only the cancelled run's reservation is released after verified termination"
    );
    let audit_tools: Vec<String> = runtime
        .list_audit_events(None, None, None, None, 20)
        .expect("list cancellation audits")
        .into_iter()
        .filter_map(|event| event.tool_name)
        .collect();
    for expected in [
        super::super::actions::CANCELLATION_REQUEST_AUDIT,
        super::super::actions::CANCELLATION_WORKER_EXIT_AUDIT,
        super::super::actions::CANCELLATION_SIGNAL_AUDIT,
        super::super::actions::CANCELLATION_COMPLETION_AUDIT,
    ] {
        assert!(
            audit_tools.iter().any(|tool| tool == expected),
            "{expected}"
        );
    }
}

#[test]
fn natural_success_racing_cancellation_remains_success() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_cancel_success_race");
    mark_running(&runtime, &run);

    let result = runtime
        .cancel_job_run_with_signaller(&run.run_id, "tester", "unit", |_| {
            runtime
                .finalize_job_run(&run.run_id, JobRunState::Success, Utc::now(), Some(1))
                .expect("worker succeeds");
            Ok("already_exited".to_string())
        })
        .expect("observe terminal winner");

    assert_eq!(result.outcome, "already_terminal");
    assert_eq!(result.final_state, "success");
    assert_eq!(
        runtime.show_job_run(&run.run_id).expect("show run").state,
        JobRunState::Success
    );
    assert!(!has_conflict(&runtime, &run.run_id));
}

#[test]
fn real_failure_racing_cancellation_remains_failed() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_cancel_failure_race");
    mark_running(&runtime, &run);

    let result = runtime
        .cancel_job_run_with_signaller(&run.run_id, "tester", "unit", |_| {
            runtime
                .finalize_job_run(&run.run_id, JobRunState::Failed, Utc::now(), Some(1))
                .expect("worker fails");
            Ok("already_exited".to_string())
        })
        .expect("observe terminal winner");

    assert_eq!(result.outcome, "already_terminal");
    assert_eq!(result.final_state, "failed");
    assert_eq!(
        runtime.show_job_run(&run.run_id).expect("show run").state,
        JobRunState::Failed
    );
    assert!(!has_conflict(&runtime, &run.run_id));
}

#[test]
fn duplicate_cancellation_requests_converge_without_conflict() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_cancel_duplicate_race");
    mark_running(&runtime, &run);

    let first = runtime
        .cancel_job_run_with_signaller(&run.run_id, "first", "unit", |_| {
            let second = runtime
                .cancel_job_run_with_signaller(&run.run_id, "second", "unit", |_| {
                    Ok("terminated_process_group".to_string())
                })
                .expect("second cancellation wins");
            assert_eq!(second.outcome, "cancelled");
            Ok("already_exited".to_string())
        })
        .expect("first cancellation observes winner");

    assert_eq!(first.outcome, "already_terminal");
    assert_eq!(first.final_state, "cancelled");
    assert!(!has_conflict(&runtime, &run.run_id));
}

#[test]
fn worker_failure_after_committed_cancellation_remains_an_explicit_conflict() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_cancel_genuine_conflict");
    runtime.cancel_job_run(&run.run_id).expect("cancel run");

    runtime
        .finalize_job_run(&run.run_id, JobRunState::Failed, Utc::now(), Some(1))
        .expect("deliver contradictory worker result");

    assert!(has_conflict(&runtime, &run.run_id));
}
