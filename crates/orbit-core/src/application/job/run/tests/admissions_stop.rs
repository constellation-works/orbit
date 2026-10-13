//! Deterministic first-checkpoint and terminalization races for admissions stop.

use crate::application::job::DrainAdmissionsStopOutcome;
use std::cell::Cell;
use std::rc::Rc;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::workflow::JobRunState;

use super::super::admissions_stop::{
    DrainAdmissionsStopRequest, set_admissions_stop_state_init_hook,
};
use super::{
    assert_worker_checkpoint_preserved, running_run_without_state, test_runtime, worker_checkpoint,
};
use crate::application::tests::run_isolated_test;

fn request() -> DrainAdmissionsStopRequest<'static> {
    DrainAdmissionsStopRequest {
        actor: "operator",
        source: "fixture",
        reason: Some("close admissions"),
        claim_token: None,
        force: false,
    }
}

#[test]
fn admissions_stop_initialization_preserves_worker_checkpoint_and_merges_control() {
    if run_isolated_test(std::any::type_name_of_val(
        &admissions_stop_initialization_preserves_worker_checkpoint_and_merges_control,
    )) {
        return;
    }
    for worker_wins in [false, true] {
        let (_root, runtime) = test_runtime();
        let run = running_run_without_state(&runtime, "workspace_auto_pipeline");
        let checkpoint = worker_wins.then(|| worker_checkpoint(&runtime, &run));
        let reached = Rc::new(Cell::new(false));
        let worker = runtime.clone();
        let run_id = run.run_id.clone();
        let progress = checkpoint.clone();
        let observed = reached.clone();
        set_admissions_stop_state_init_hook(move || {
            assert!(!observed.replace(true), "NoState seam must run once");
            assert!(worker.read_run_state(&run_id).unwrap().is_none());
            if let Some(state) = progress {
                assert!(
                    worker
                        .stores()
                        .jobs()
                        .initialize_run_state(&run_id, &state)
                        .unwrap()
                );
            }
        });

        let result = runtime.stop_workspace_auto_admissions(request()).unwrap();
        assert!(reached.get(), "force the NoState initialization window");
        assert_eq!(result.outcome, DrainAdmissionsStopOutcome::Stopped);
        assert_eq!(result.coordinators.len(), 1);
        let state = runtime.read_run_state(&run.run_id).unwrap().unwrap();
        let stop = state.pull_drain.drain_admissions_stop.as_ref().unwrap();
        assert_eq!(stop.actor, "operator");
        assert_eq!(stop.reason.as_deref(), request().reason);
        if let Some(checkpoint) = checkpoint {
            assert_worker_checkpoint_preserved(&state, &checkpoint);
            assert_eq!(result.coordinators[0].remaining_children.len(), 1);
            assert_eq!(
                result.coordinators[0].remaining_children[0].run_id,
                checkpoint.child_dispatches[0].child_run_id
            );
        }
        let repeated = runtime.stop_workspace_auto_admissions(request()).unwrap();
        assert_eq!(
            repeated.coordinators[0].outcome,
            DrainAdmissionsStopOutcome::Unchanged
        );
        assert_eq!(runtime.read_run_state(&run.run_id).unwrap().unwrap(), state);
    }
}

#[test]
fn admissions_stop_initialization_refuses_concurrent_terminalization() {
    if run_isolated_test(std::any::type_name_of_val(
        &admissions_stop_initialization_refuses_concurrent_terminalization,
    )) {
        return;
    }
    for terminal in [JobRunState::Success, JobRunState::Failed] {
        for worker_wins in [false, true] {
            let (_root, runtime) = test_runtime();
            let run = running_run_without_state(&runtime, "workspace_auto_pipeline");
            let checkpoint = worker_wins.then(|| worker_checkpoint(&runtime, &run));
            let worker = runtime.clone();
            let run_id = run.run_id.clone();
            let progress = checkpoint.clone();
            set_admissions_stop_state_init_hook(move || {
                assert!(worker.read_run_state(&run_id).unwrap().is_none());
                if let Some(state) = progress {
                    assert!(
                        worker
                            .stores()
                            .jobs()
                            .initialize_run_state(&run_id, &state)
                            .unwrap()
                    );
                }
                assert!(
                    worker
                        .stores()
                        .jobs()
                        .finalize_job_run(&run_id, terminal, Utc::now(), None)
                        .unwrap()
                );
            });

            let error = runtime
                .stop_workspace_auto_admissions(request())
                .unwrap_err();
            assert!(
                matches!(error, OrbitError::JobValidation(ref message) if message.contains("terminal run admits no further work")),
                "fresh terminal state must refuse admissions stop: {error}"
            );
            assert_eq!(
                runtime
                    .get_job_run_backend(&run.run_id)
                    .unwrap()
                    .unwrap()
                    .state,
                terminal
            );
            let state = runtime.read_run_state(&run.run_id).unwrap().unwrap();
            assert!(
                !state.admissions_stopped(),
                "a terminal drain must receive no new control"
            );
            if let Some(mut checkpoint) = checkpoint {
                checkpoint.compact_for_terminal(terminal);
                assert_worker_checkpoint_preserved(&state, &checkpoint);
            }
        }
    }
}
