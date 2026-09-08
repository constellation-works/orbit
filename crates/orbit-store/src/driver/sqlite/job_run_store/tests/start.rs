use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::workflow::{JobRunStartOutcome, JobRunState, PipelineState};
use tempfile::TempDir;

use super::super::SqliteJobRunStore;
use crate::Store;
use crate::contracts::JobRunStoreBackend;

/// [ORB-10965] At-least-once delivery redelivers a Start to the worker
/// that already owns the run. That is a no-op, not a state-machine
/// violation: no timestamp, owner identity, or checkpoint moves.
#[test]
fn duplicate_start_from_the_same_owner_is_a_no_op() {
    let backend = SqliteJobRunStore::new(Store::open_in_memory().expect("store"), "ws_a");
    let pid = std::process::id();
    let run = backend
        .insert_job_run("job-dup", 1, Utc::now(), None, None)
        .expect("insert");

    let first_started_at = Utc::now();
    assert_eq!(
        backend
            .mark_job_run_running(&run.run_id, first_started_at, pid)
            .expect("first start"),
        JobRunStartOutcome::Started
    );
    let claimed = backend
        .get_job_run(&run.run_id)
        .expect("get")
        .expect("some");

    let mut state = PipelineState::new(
        run.run_id.clone(),
        run.job_id.clone(),
        serde_json::json!({"seconds": 0}),
    );
    state.record_step(0, JobRunState::Success, None, None);
    backend
        .write_run_state(&run.run_id, &state)
        .expect("write checkpoint");

    // The redelivery carries a later timestamp; the incumbent's stays.
    let outcome = backend
        .mark_job_run_running(&run.run_id, first_started_at + Duration::from_secs(30), pid)
        .expect("redelivered start succeeds");
    assert_eq!(outcome, JobRunStartOutcome::AlreadyStarted);
    assert!(
        !outcome.owns_execution(),
        "a redelivery must not grant a second execution"
    );

    let after = backend
        .get_job_run(&run.run_id)
        .expect("get")
        .expect("some");
    assert_eq!(after.state, JobRunState::Running);
    assert_eq!(after.started_at, claimed.started_at);
    assert_eq!(after.pid, Some(pid));
    assert_eq!(after.pid_start_time, claimed.pid_start_time);
    assert_eq!(
        backend
            .read_run_state(&run.run_id)
            .expect("read checkpoint")
            .expect("checkpoint survives")
            .step_states
            .get(&0),
        Some(&JobRunState::Success),
        "checkpoints must survive a deduplicated start"
    );
}

/// [ORB-10965] A duplicate Start whose owner differs from the incumbent's
/// is reported as a conflict naming both, not as a generic transition
/// error, so the loser can yield instead of failing the run.
#[test]
fn duplicate_start_from_a_different_owner_is_a_specific_conflict() {
    let backend = SqliteJobRunStore::new(Store::open_in_memory().expect("store"), "ws_a");
    let incumbent = std::process::id();
    let run = backend
        .insert_job_run("job-conflict", 1, Utc::now(), None, None)
        .expect("insert");
    let started_at = Utc::now();
    backend
        .mark_job_run_running(&run.run_id, started_at, incumbent)
        .expect("incumbent start");

    let challenger = incumbent + 1;
    let error = backend
        .mark_job_run_running(&run.run_id, Utc::now(), challenger)
        .expect_err("a competing owner has no authority");
    let message = match &error {
        OrbitError::JobRunStartConflict(message) => message.clone(),
        other => panic!("expected a start conflict, got {other:?}"),
    };
    assert!(message.contains(&incumbent.to_string()), "{message}");
    assert!(message.contains(&challenger.to_string()), "{message}");

    let after = backend
        .get_job_run(&run.run_id)
        .expect("get")
        .expect("some");
    assert_eq!(after.state, JobRunState::Running);
    assert_eq!(after.pid, Some(incumbent));

    // The same rule holds once the run has finished: a late duplicate
    // cannot reopen a terminal run, and the refusal still names the race.
    backend
        .finalize_job_run(&run.run_id, JobRunState::Success, Utc::now(), Some(5))
        .expect("finalize");
    assert!(matches!(
        backend.mark_job_run_running(&run.run_id, Utc::now(), challenger),
        Err(OrbitError::JobRunStartConflict(_))
    ));
    assert_eq!(
        backend
            .mark_job_run_running(&run.run_id, Utc::now(), incumbent)
            .expect("owner redelivery after completion"),
        JobRunStartOutcome::AlreadyStarted
    );
    assert_eq!(
        backend
            .get_job_run(&run.run_id)
            .expect("get")
            .expect("some")
            .state,
        JobRunState::Success,
        "a duplicate start must not resurrect a finished run"
    );
}

/// [ORB-10965] Deduplication is scoped to states a Start can legitimately
/// race with. A Start from `retrying` is still a genuine state-machine
/// violation and keeps rejecting.
#[test]
fn start_from_an_illegal_state_still_rejects_as_a_transition_error() {
    let store = Store::open_in_memory().expect("store");
    let backend = SqliteJobRunStore::new(store.clone(), "ws_a");
    let run = backend
        .insert_job_run("job-illegal", 1, Utc::now(), None, None)
        .expect("insert");
    let mut retrying = run.clone();
    retrying.state = JobRunState::Retrying;
    store
        .upsert_job_run_for_workspace("ws_a", &retrying, None)
        .expect("force retrying state");

    assert!(matches!(
        backend.mark_job_run_running(&run.run_id, Utc::now(), std::process::id()),
        Err(OrbitError::JobRunStateTransition(_))
    ));
}

/// [ORB-10965] The duplicate-delivery race itself: two workers hand the
/// same queued run to the store at the same instant. The immediate
/// transaction serializes them, so exactly one is granted execution.
#[test]
fn competing_starts_grant_execution_authority_to_exactly_one_worker() {
    let temp = TempDir::new().expect("tempdir");
    let db_path = temp.path().join("orbit.db");
    let backend_a = SqliteJobRunStore::new(Store::open(&db_path).expect("store a"), "ws_a");
    let backend_b = SqliteJobRunStore::new(Store::open(&db_path).expect("store b"), "ws_a");
    let run = backend_a
        .insert_job_run("job-race", 1, Utc::now(), None, None)
        .expect("insert");
    let run_id = run.run_id.clone();
    let barrier = Arc::new(Barrier::new(2));

    let mut workers = Vec::new();
    for (backend, pid) in [(backend_a, 100_001_u32), (backend_b, 100_002_u32)] {
        let run_id = run_id.clone();
        let barrier = Arc::clone(&barrier);
        workers.push(thread::spawn(move || {
            barrier.wait();
            (pid, backend.mark_job_run_running(&run_id, Utc::now(), pid))
        }));
    }
    let results: Vec<(u32, Result<JobRunStartOutcome, OrbitError>)> = workers
        .into_iter()
        .map(|worker| worker.join().expect("worker"))
        .collect();

    let winners: Vec<u32> = results
        .iter()
        .filter(|(_, result)| matches!(result, Ok(outcome) if outcome.owns_execution()))
        .map(|(pid, _)| *pid)
        .collect();
    assert_eq!(
        winners.len(),
        1,
        "exactly one worker may advance: {results:?}"
    );
    assert!(
        results.iter().any(|(pid, result)| *pid != winners[0]
            && matches!(result, Err(OrbitError::JobRunStartConflict(_)))),
        "the loser must yield with a start conflict: {results:?}"
    );

    let stored = SqliteJobRunStore::new(Store::open(&db_path).expect("store c"), "ws_a")
        .get_job_run(&run_id)
        .expect("read")
        .expect("run");
    assert_eq!(stored.state, JobRunState::Running);
    assert_eq!(stored.pid, Some(winners[0]));
}
