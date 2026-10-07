//! Forced local drain cancellation preserves the operator's task disposition.

use chrono::Utc;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{DrainAdmissionsStopRequest, OrbitRuntime};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::{TaskComplexity, TaskStatus};
use orbit_types::workflow::{ChildDispatch, JobRunState, PipelineState};
use serde_json::json;

#[test]
fn forced_local_cancel_and_admissions_stop_preserve_task_disposition() {
    if !super::dispatch_admission::isolated(
        "drain_cancel::forced_local_cancel_and_admissions_stop_preserve_task_disposition",
    ) {
        return;
    }
    // The direct operator path accepts an explicit block policy. The
    // admissions-stop control defaults to backlog for every cancelled task.
    for (stop_admissions, block, expected) in [
        (false, false, TaskStatus::Backlog),
        (false, true, TaskStatus::Blocked),
        (true, false, TaskStatus::Backlog),
    ] {
        let root = tempfile::tempdir().unwrap();
        let global = root.path().join("global");
        let workspace = root.path().join("repo/.orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
        let jobs = orbit_store::compose::workspace_job_run_store(
            runtime.sqlite_store().unwrap(),
            runtime.workspace_id().unwrap(),
        );
        let parent = jobs
            .insert_job_run(
                "workspace_auto_pipeline",
                1,
                Utc::now(),
                Some(json!({})),
                None,
            )
            .unwrap();
        let child = jobs
            .insert_job_run("task_auto_pipeline", 1, Utc::now(), None, None)
            .unwrap();
        let task = runtime
            .add_task(TaskAddParams {
                title: "Cancelled local drain candidate".into(),
                plan: "Resume the existing candidate".into(),
                complexity: TaskComplexity::Low,
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            })
            .unwrap();
        runtime
            .apply_task_automation_update(
                &task.id,
                TaskAutomationUpdate {
                    status: Some(TaskStatus::InProgress),
                    job_run_id: Some(child.run_id.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        let mut state = PipelineState::new(parent.run_id.clone(), parent.job_id.clone(), json!({}));
        state.record_child_dispatch(ChildDispatch::submitted(
            child.run_id.clone(),
            child.job_id.clone(),
            "invoke_detached".into(),
            false,
            false,
            Utc::now(),
        ));
        // A persisted state distinguishes an admitted coordinator from a
        // never-started queued run, without starting a real worker.
        jobs.write_run_state(&parent.run_id, &state).unwrap();

        let reason = "host maintenance";
        let forced_runs = if stop_admissions {
            let stopped = runtime
                .stop_workspace_auto_admissions(DrainAdmissionsStopRequest {
                    actor: "operator",
                    source: "fixture",
                    reason: Some(reason),
                    claim_token: None,
                    force: true,
                })
                .unwrap();
            assert_eq!(stopped.outcome, "force_cancelled");
            assert_eq!(stopped.coordinators.len(), 1);
            let change = &stopped.coordinators[0];
            assert!(change.remaining_children.is_empty());
            assert!(change.unstopped_children.is_empty());
            change.forced_runs.clone()
        } else {
            let cancelled = runtime
                .cancel_job_run_with_options_and_policy(
                    &parent.run_id,
                    "operator",
                    "fixture",
                    Some(reason),
                    true,
                    block,
                )
                .unwrap();
            assert_eq!(cancelled.outcome, "cancelled");
            assert!(cancelled.unstopped_children.is_empty());
            cancelled.forced_runs
        };

        assert_eq!(forced_runs.as_slice(), std::slice::from_ref(&child.run_id));
        for run in [&parent.run_id, &child.run_id] {
            assert_eq!(
                jobs.get_job_run(run).unwrap().unwrap().state,
                JobRunState::Cancelled
            );
        }
        let task = runtime.get_task(&task.id).unwrap();
        assert_eq!(
            task.status, expected,
            "forced cancellation must honor the task policy"
        );
        assert_eq!(task.plan, "Resume the existing candidate");
        assert_eq!(task.job_run_id.as_deref(), Some(child.run_id.as_str()));
        let policy = jobs
            .read_run_state(&child.run_id)
            .unwrap()
            .unwrap()
            .task_cancellation_policy
            .unwrap();
        assert_eq!(policy.block, block);
        assert!(policy.note.contains(reason));
    }
}
