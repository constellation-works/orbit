//! [ORB-11283] Stop new auto admissions without cancelling children.

use chrono::Utc;
use orbit_types::workflow::{
    ChildDispatch, ChildDispatchPhase, JobRun, JobRunState, PipelineState,
};
use serde_json::json;

use super::*;
use crate::application::job::DrainAdmissionsStopRequest;

const DRAIN_JOB: &str = "workspace_auto_pipeline";

fn running_drain(runtime: &OrbitRuntime) -> JobRun {
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(DRAIN_JOB, 1, Utc::now(), Some(json!({})), None)
        .expect("insert drain run");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("start drain run");
    let state = PipelineState::new(
        run.run_id.clone(),
        DRAIN_JOB.to_string(),
        run.input.clone().unwrap_or_else(|| json!({})),
    );
    runtime
        .stores()
        .jobs()
        .write_run_state(&run.run_id, &state)
        .expect("write drain state");
    runtime.show_job_run(&run.run_id).expect("reload drain run")
}

fn request<'a>() -> DrainAdmissionsStopRequest<'a> {
    DrainAdmissionsStopRequest {
        actor: "tester",
        source: "unit",
        reason: Some("window closed early"),
        claim_token: None,
    }
}

#[test]
fn stopping_a_running_drain_writes_the_control_and_leaves_children_running() {
    let (_root, runtime) = test_runtime();
    let run = running_drain(&runtime);
    let child = runtime
        .stores()
        .jobs()
        .insert_job_run(
            "task_auto_pipeline",
            1,
            Utc::now(),
            Some(json!({ "task_ids": ["ORB-1"] })),
            None,
        )
        .expect("insert child");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&child.run_id, Utc::now(), std::process::id())
        .expect("start child");
    let mut state = runtime
        .read_run_state(&run.run_id)
        .expect("read state")
        .expect("state exists");
    state.record_child_dispatch(ChildDispatch::submitted(
        child.run_id.clone(),
        "task_auto_pipeline".to_string(),
        "invoke_detached".to_string(),
        false,
        false,
        Utc::now(),
    ));
    runtime
        .write_run_state(&run.run_id, &state)
        .expect("link child");

    let result = runtime
        .stop_workspace_auto_admissions(request())
        .expect("stop drain");

    assert_eq!(result.outcome, "stopped");
    assert_eq!(result.coordinators.len(), 1);
    assert_eq!(result.coordinators[0].run_id, run.run_id);
    assert_eq!(result.coordinators[0].outcome, "stopped");
    assert_eq!(result.coordinators[0].remaining_children.len(), 1);
    assert_eq!(
        result.coordinators[0].remaining_children[0].run_id,
        child.run_id
    );

    let reloaded = runtime.show_job_run(&run.run_id).expect("reload parent");
    assert_eq!(reloaded.state, JobRunState::Running);
    let stored = runtime
        .read_run_state(&run.run_id)
        .expect("read state")
        .expect("state exists");
    assert!(stored.admissions_stopped());
    assert_eq!(
        stored.drain_admissions_stop.as_ref().expect("stop").actor,
        "tester"
    );
    let child = runtime.show_job_run(&child.run_id).expect("reload child");
    assert_eq!(child.state, JobRunState::Running);
    assert_eq!(
        stored.child_dispatches[0].phase,
        ChildDispatchPhase::Submitted
    );
}

#[test]
fn repeated_stop_and_no_active_run_are_idempotent() {
    let (_root, runtime) = test_runtime();

    let idle = runtime
        .stop_workspace_auto_admissions(request())
        .expect("idle stop");
    assert_eq!(idle.outcome, "idle");
    assert!(idle.coordinators.is_empty());

    let run = running_drain(&runtime);
    runtime
        .stop_workspace_auto_admissions(request())
        .expect("first stop");
    let repeated = runtime
        .stop_workspace_auto_admissions(request())
        .expect("second stop");
    assert_eq!(repeated.outcome, "unchanged");
    assert_eq!(repeated.coordinators[0].run_id, run.run_id);
    assert_eq!(repeated.coordinators[0].outcome, "unchanged");
}

#[test]
fn a_queued_drain_is_cancelled_before_it_can_admit() {
    let (_root, runtime) = test_runtime();
    let pending = insert_pending_run(&runtime, DRAIN_JOB);

    let result = runtime
        .stop_workspace_auto_admissions(request())
        .expect("stop queued drain");

    assert_eq!(result.outcome, "cancelled_queued");
    assert_eq!(result.coordinators[0].run_id, pending.run_id);
    let stored = runtime.show_job_run(&pending.run_id).expect("reload");
    assert_eq!(stored.state, JobRunState::Cancelled);
}

#[test]
fn a_leaf_run_in_the_same_workspace_is_not_stopped() {
    let (_root, runtime) = test_runtime();
    let leaf = insert_pending_run(&runtime, "task_auto_pipeline");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&leaf.run_id, Utc::now(), std::process::id())
        .expect("start leaf");

    let result = runtime
        .stop_workspace_auto_admissions(request())
        .expect("stop with no drain");

    assert_eq!(result.outcome, "idle");
    let stored = runtime.show_job_run(&leaf.run_id).expect("reload leaf");
    assert_eq!(stored.state, JobRunState::Running);
}

#[test]
fn a_finished_child_is_not_listed_as_remaining() {
    let (_root, runtime) = test_runtime();
    let run = running_drain(&runtime);
    let child = runtime
        .stores()
        .jobs()
        .insert_job_run("task_auto_pipeline", 1, Utc::now(), None, None)
        .expect("insert child");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&child.run_id, Utc::now(), std::process::id())
        .expect("start child");
    runtime
        .stores()
        .jobs()
        .finalize_job_run(&child.run_id, JobRunState::Success, Utc::now(), Some(1))
        .expect("finish child");
    let mut state = runtime
        .read_run_state(&run.run_id)
        .expect("read state")
        .expect("state exists");
    state.record_child_dispatch(ChildDispatch::submitted(
        child.run_id.clone(),
        "task_auto_pipeline".to_string(),
        "invoke_detached".to_string(),
        false,
        false,
        Utc::now(),
    ));
    runtime
        .write_run_state(&run.run_id, &state)
        .expect("link child");

    let result = runtime
        .stop_workspace_auto_admissions(request())
        .expect("stop drain");
    assert!(result.coordinators[0].remaining_children.is_empty());
}
