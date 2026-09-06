//! [ORB-10597] A terminal outcome that contradicts the one already persisted
//! must be recorded, not silently dropped.
//!
//! Reproduces the shape of `jrun-20260802-2013-2`: a run marked `interrupted`
//! while it was still working, which then ran to a genuine success. Before this
//! change the success was discarded with no error, no warning, and nothing in
//! the durable record, leaving the run record permanently contradicting its own
//! steps and audit trail.

use super::*;

use chrono::Duration;
use orbit_store::contracts::TaskReservationReleaseReason;

use crate::application::job::TERMINAL_OUTCOME_CONFLICT_CODE;

/// Drive a run to `state`, as the owning worker would.
fn finalize(
    runtime: &OrbitRuntime,
    run: &JobRun,
    state: JobRunState,
    finished_at: DateTime<Utc>,
) -> bool {
    runtime
        .finalize_job_run_with_reservation_cleanup(
            &run.run_id,
            state,
            finished_at,
            Some(1_000),
            TaskReservationReleaseReason::RunTerminal,
        )
        .expect("finalize run")
}

fn conflict_step(runtime: &OrbitRuntime, run_id: &str) -> Option<String> {
    runtime
        .show_job_run(run_id)
        .expect("show run")
        .steps
        .into_iter()
        .find(|step| step.error_code.as_deref() == Some(TERMINAL_OUTCOME_CONFLICT_CODE))
        .and_then(|step| step.error_message)
}

/// The incident: condemned to `interrupted`, then the run really succeeds.
#[test]
fn success_after_a_false_interrupt_is_recorded_not_discarded() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_conflict");
    let started_at = Utc::now() - Duration::seconds(60);
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, started_at, std::process::id())
        .expect("mark running");

    let condemned_at = started_at + Duration::seconds(10);
    finalize(&runtime, &run, JobRunState::Interrupted, condemned_at);

    // The run keeps working and reports the success it actually reached.
    let really_finished_at = started_at + Duration::seconds(50);
    finalize(&runtime, &run, JobRunState::Success, really_finished_at);

    let shown = runtime.show_job_run(&run.run_id).expect("show run");
    // The recorded terminal state stands: the first terminalization already
    // fired irreversible side effects, and cancellation must not be revocable.
    assert_eq!(shown.state, JobRunState::Interrupted);
    assert_eq!(shown.finished_at, Some(condemned_at));

    // ...but the outcome that lost is preserved on the run, where a reader
    // looking at the contradictory state will find it.
    let message = conflict_step(&runtime, &run.run_id).expect("conflict step recorded on the run");
    assert!(
        message.contains("conflicting terminal outcome"),
        "conflict step must announce itself: {message}"
    );
    for expected in [
        "interrupted",
        "success",
        &condemned_at.to_rfc3339(),
        &really_finished_at.to_rfc3339(),
    ] {
        assert!(
            message.contains(expected),
            "conflict record must name both outcomes and both finish times, missing {expected}: {message}"
        );
    }
}

/// The conflict is recorded once, however often the losing outcome is
/// re-delivered — a retrying worker must not append a step per attempt.
#[test]
fn repeated_conflicting_finalizations_record_one_conflict() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_conflict_repeat");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("mark running");

    let finished_at = Utc::now();
    finalize(&runtime, &run, JobRunState::Interrupted, finished_at);
    finalize(&runtime, &run, JobRunState::Success, finished_at);
    finalize(&runtime, &run, JobRunState::Success, finished_at);
    finalize(&runtime, &run, JobRunState::Failed, finished_at);

    let conflicts = runtime
        .show_job_run(&run.run_id)
        .expect("show run")
        .steps
        .into_iter()
        .filter(|step| step.error_code.as_deref() == Some(TERMINAL_OUTCOME_CONFLICT_CODE))
        .count();
    assert_eq!(conflicts, 1, "conflict is recorded once per run");
}

/// An identical re-finalization is an ordinary idempotent replay — several
/// production paths do it — and must not be reported as a conflict.
#[test]
fn identical_refinalization_is_not_a_conflict() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_conflict_replay");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("mark running");

    let finished_at = Utc::now();
    finalize(&runtime, &run, JobRunState::Success, finished_at);
    finalize(
        &runtime,
        &run,
        JobRunState::Success,
        finished_at + Duration::seconds(5),
    );

    let shown = runtime.show_job_run(&run.run_id).expect("show run");
    assert_eq!(shown.state, JobRunState::Success);
    assert_eq!(shown.finished_at, Some(finished_at));
    assert!(
        conflict_step(&runtime, &run.run_id).is_none(),
        "an idempotent replay is not a conflicting outcome"
    );
}

/// A cancellation request can lose the terminalization race after its final
/// state snapshot. That is an already-terminal result, not evidence that the
/// worker produced two contradictory outcomes.
#[test]
fn cancelled_report_after_worker_terminalization_is_not_a_conflict() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_cancel_lost_race");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("mark running");

    let finished_at = Utc::now();
    finalize(&runtime, &run, JobRunState::Success, finished_at);
    finalize(
        &runtime,
        &run,
        JobRunState::Cancelled,
        finished_at + Duration::milliseconds(1),
    );

    let shown = runtime.show_job_run(&run.run_id).expect("show run");
    assert_eq!(shown.state, JobRunState::Success);
    assert!(
        conflict_step(&runtime, &run.run_id).is_none(),
        "a losing cancellation request must not manufacture a conflict"
    );
}
