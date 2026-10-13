//! Cancellation preserves worker checkpoints and lineage at injected write boundaries.

use chrono::Utc;
use orbit_types::workflow::{ChildDispatch, JobRun, JobRunState, PipelineState, RunStateUpdate};
use serde_json::json;

use super::super::actions::set_cancellation_state_write_hook;
use super::test_runtime;
use crate::OrbitRuntime;
use crate::application::tests::run_isolated_test;

fn detached_child(id: &str) -> ChildDispatch {
    ChildDispatch::submitted(
        id.into(),
        "child".into(),
        "invoke_detached".into(),
        false,
        false,
        Utc::now(),
    )
}

/// Commit worker progress precisely at the selected cancellation write boundary.
/// The old read/write pair has already read its snapshot here; a transactional
/// updater has not yet read its locked snapshot. No sleeps or scheduler races.
fn commit_worker_at_write(runtime: OrbitRuntime, run: JobRun, remaining: usize) {
    set_cancellation_state_write_hook(move || {
        if remaining > 1 {
            commit_worker_at_write(runtime, run, remaining - 1);
            return;
        }
        let mut checkpoint = |_: JobRunState, state: &mut PipelineState| {
            state.record_step(
                4,
                JobRunState::Success,
                Some(json!({"commit": "worker-commit"})),
                Some(json!({"worker_progress": true})),
            );
            state.record_pipeline_output("worker_commit", json!({"commit": "worker-commit"}));
            state.record_child_dispatch(detached_child("worker-child"));
            Ok(())
        };
        match runtime
            .stores()
            .jobs()
            .update_run_state(&run.run_id, &mut checkpoint)
            .expect("worker transaction")
        {
            RunStateUpdate::Updated => {}
            RunStateUpdate::NoState => {
                let mut state = PipelineState::new(
                    run.run_id.clone(),
                    run.job_id.clone(),
                    run.input.clone().unwrap_or_else(|| json!({})),
                );
                checkpoint(run.state, &mut state).unwrap();
                assert!(
                    runtime
                        .stores()
                        .jobs()
                        .initialize_run_state(&run.run_id, &state)
                        .expect("worker first checkpoint")
                );
            }
            RunStateUpdate::NotFound => panic!("worker run disappeared"),
        }
    });
}

#[test]
fn cancellation_writes_preserve_interleaved_worker_checkpoints_and_lineage() {
    if run_isolated_test(std::any::type_name_of_val(
        &cancellation_writes_preserve_interleaved_worker_checkpoints_and_lineage,
    )) {
        return;
    }

    for (case, initial_state, worker_write) in [
        ("policy", true, 1),
        ("initialization", false, 2),
        ("terminal state", true, 2),
        ("child settlement", true, 3),
        ("no worker checkpoint", false, 0),
    ] {
        let (_root, runtime) = test_runtime();
        let run = runtime
            .stores()
            .jobs()
            .insert_job_run(
                "cancel_interleaving",
                1,
                Utc::now(),
                Some(json!({"input": 7})),
                None,
            )
            .unwrap();
        if initial_state {
            let mut state = PipelineState::new(
                run.run_id.clone(),
                run.job_id.clone(),
                run.input.clone().unwrap(),
            );
            state.record_child_dispatch(detached_child("seed-child"));
            runtime.write_run_state(&run.run_id, &state).unwrap();
        }
        runtime
            .stores()
            .jobs()
            .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
            .unwrap();
        if worker_write != 0 {
            commit_worker_at_write(runtime.clone(), run.clone(), worker_write);
        }

        let result = runtime
            .cancel_job_run_with_reason_and_policy_and_signal(
                &run.run_id,
                "operator",
                "test",
                Some("cancel work"),
                false,
                |_| {
                    let state = runtime.read_run_state(&run.run_id)?.unwrap();
                    let policy = state.task_cancellation_policy.unwrap();
                    assert!(
                        !policy.block,
                        "policy must be durable before signalling: {case}"
                    );
                    assert!(
                        !policy.note.is_empty(),
                        "cancellation note is required: {case}"
                    );
                    Ok("owner_stopped".into())
                },
            )
            .expect(case);
        assert_eq!(result.final_state, "cancelled", "{case}");
        let state = runtime.read_run_state(&run.run_id).unwrap().unwrap();
        assert_eq!(state.initial_input, json!({"input": 7}), "{case}");
        assert_eq!(state.pipeline["cancelled"], true, "{case}");
        assert!(
            !state.task_cancellation_policy.as_ref().unwrap().block,
            "{case}"
        );
        if worker_write != 0 {
            assert_eq!(
                state.step_output(4),
                Some(&json!({"commit": "worker-commit"})),
                "{case}: cancellation must preserve the worker checkpoint"
            );
            assert!(
                state.step_outputs.is_empty() && state.pipeline_patches.is_empty(),
                "{case}: a cancelled run keeps no resume-only maps"
            );
            assert_eq!(state.step_states[&4], JobRunState::Success, "{case}");
            assert_eq!(state.pipeline["worker_progress"], true, "{case}");
            assert!(
                state
                    .child_dispatches
                    .iter()
                    .any(|child| child.child_run_id == "worker-child"),
                "{case}: cancellation must preserve worker child lineage"
            );
        } else {
            assert!(state.step_outputs.is_empty(), "{case}");
        }
    }
}
