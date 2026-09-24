//! Store-read behaviour for `show_job_run`.

use super::*;

use crate::application::job::run::job_run_get_counter;
use orbit_types::workflow::JobRunState;

#[test]
fn show_job_run_performs_one_store_read_when_reconciliation_makes_no_change() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_show_once");
    let reads = job_run_get_counter::track(&runtime, &run.run_id);

    let shown = runtime.show_job_run(&run.run_id).expect("show pending run");

    assert_eq!(shown.state, JobRunState::Pending);
    assert_eq!(
        reads.reads(),
        1,
        "show_job_run must keep the first snapshot when reconcile reports no change"
    );
}

#[cfg(unix)]
#[test]
fn show_job_run_performs_one_store_read_for_a_live_running_owner() {
    use orbit_common::process::identity::process_start_identity_token;

    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_show_live_once");
    let pid = std::process::id();
    if process_start_identity_token(pid).is_none() {
        return;
    }
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, chrono::Utc::now(), pid)
        .expect("mark current process running");
    let reads = job_run_get_counter::track(&runtime, &run.run_id);

    let shown = runtime.show_job_run(&run.run_id).expect("show live run");

    assert_eq!(shown.state, JobRunState::Running);
    assert_eq!(
        reads.reads(),
        1,
        "a healthy running owner must not trigger a second store read"
    );
}

/// The observed reads report stale `pending`/`running` runs exactly as stored
/// and leave their reservations held; the reconciling reads operators use
/// still finalize the same runs [ORB-12941].
#[test]
fn observed_reads_leave_stale_runs_and_reservations_untouched() {
    use super::super::JobRunListParams;
    use chrono::{Duration, Utc};

    let (_root, runtime) = test_runtime();
    let running = insert_pending_run(&runtime, "qa_observe_running");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&running.run_id, Utc::now() - Duration::hours(2), 999_999)
        .expect("mark running with impossible pid");
    let pending = insert_pending_run(&runtime, "qa_observe_pending");
    backdate_run_created_at(&runtime, &pending, Utc::now() - Duration::days(4));
    reserve_for_run(&runtime, &running.run_id, "src/running.rs");
    reserve_for_run(&runtime, &pending.run_id, "src/pending.rs");
    let state_of = |runs: &[JobRun], run_id: &str| {
        runs.iter()
            .find(|run| run.run_id == run_id)
            .map(|run| run.state)
    };

    let listed = runtime
        .list_job_runs_observed(JobRunListParams::default())
        .expect("observed list");
    assert_eq!(
        state_of(&listed, &running.run_id),
        Some(JobRunState::Running)
    );
    assert_eq!(
        state_of(&listed, &pending.run_id),
        Some(JobRunState::Pending)
    );
    for (run, state) in [
        (&running, JobRunState::Running),
        (&pending, JobRunState::Pending),
    ] {
        let shown = runtime
            .show_job_run_observed(&run.run_id)
            .expect("observed show");
        assert_eq!(shown.state, state);
        assert!(shown.finished_at.is_none());
    }
    let mut owners = active_reservation_owners(&runtime);
    owners.sort();
    let mut expected = vec![running.run_id.clone(), pending.run_id.clone()];
    expected.sort();
    assert_eq!(
        owners, expected,
        "observed reads must not release reservations"
    );

    let reconciled = runtime
        .list_job_runs(JobRunListParams::default())
        .expect("reconciling list");
    assert_eq!(
        state_of(&reconciled, &running.run_id),
        Some(JobRunState::Interrupted)
    );
    assert_eq!(
        state_of(&reconciled, &pending.run_id),
        Some(JobRunState::Interrupted)
    );
    assert!(active_reservation_owners(&runtime).is_empty());
}
