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
