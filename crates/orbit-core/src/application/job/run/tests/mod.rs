//! Shared test helpers and child module declarations for run command tests.

use crate::OrbitRuntime;

#[cfg(unix)]
mod actions;
mod admissions_stop;
mod conflict;
mod drain_cancel;
mod owner;

use chrono::{DateTime, Utc};
use orbit_types::workflow::{ChildDispatch, JobRun, JobRunState, PipelineState};
use serde_json::json;
use tempfile::tempdir;

pub(crate) fn test_runtime() -> (tempfile::TempDir, OrbitRuntime) {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime)
}

pub(crate) fn insert_pending_run(runtime: &OrbitRuntime, job_id: &str) -> JobRun {
    runtime
        .stores()
        .jobs()
        .insert_job_run(
            job_id,
            1,
            Utc::now() - chrono::Duration::seconds(5),
            None,
            None,
        )
        .expect("insert run")
}

pub(super) fn running_run_without_state(runtime: &OrbitRuntime, job_id: &str) -> JobRun {
    let jobs = runtime.stores().jobs();
    let run = jobs
        .insert_job_run(job_id, 1, Utc::now(), Some(json!({"input": 7})), None)
        .expect("insert drain");
    jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("mark drain running before its first checkpoint");
    runtime.get_job_run_backend(&run.run_id).unwrap().unwrap()
}

pub(super) fn worker_checkpoint(runtime: &OrbitRuntime, run: &JobRun) -> PipelineState {
    let child = insert_pending_run(runtime, "child");
    let mut state = PipelineState::new(
        run.run_id.clone(),
        run.job_id.clone(),
        run.input.clone().unwrap(),
    );
    state.record_step(
        4,
        JobRunState::Success,
        Some(json!({"commit": "worker-commit"})),
        Some(json!({"worker_progress": true})),
    );
    state.record_pipeline_output("worker_commit", json!({"commit": "worker-commit"}));
    state.record_child_dispatch(ChildDispatch::submitted(
        child.run_id,
        child.job_id,
        "invoke_detached".into(),
        false,
        false,
        Utc::now(),
    ));
    state
}

pub(super) fn assert_worker_checkpoint_preserved(actual: &PipelineState, expected: &PipelineState) {
    assert_eq!(actual.initial_input, expected.initial_input);
    assert_eq!(
        actual.pipeline, expected.pipeline,
        "worker progress must survive the control"
    );
    assert_eq!(actual.step_outputs, expected.step_outputs);
    assert_eq!(actual.step_output_pointers, expected.step_output_pointers);
    assert_eq!(actual.pipeline_patches, expected.pipeline_patches);
    assert_eq!(actual.step_states, expected.step_states);
    assert_eq!(actual.next_step_index, expected.next_step_index);
    assert_eq!(actual.previous_step_state, expected.previous_step_state);
    assert_eq!(
        actual.child_dispatches, expected.child_dispatches,
        "worker child lineage must survive the control"
    );
}
