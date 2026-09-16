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
