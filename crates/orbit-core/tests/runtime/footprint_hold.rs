//! A resumed run whose deferred footprint stays held past its wait ends
//! held through the public worker and its real run finalizer: no editor
//! starts, the run does not fail, and its task returns to the backlog for a
//! later admission. `footprint_wait_seconds: 0` makes the wait expire on
//! the first poll, so nothing here sleeps or depends on load.

use chrono::Utc;
use orbit_core::TaskStatus;
use orbit_engine::{DispatchError, RuntimeHost, TaskAutomationUpdate};
use orbit_types::resource::ExecutorResource;
use orbit_types::workflow::activity_job::Provider;
use orbit_types::workflow::{
    ExecutorDef, ExecutorSandboxKind, FOOTPRINT_UNAVAILABLE_ERROR_CODE, HeldFailure, JobRunState,
    PipelineState,
};
use serde_json::json;

use super::provider_failure_hold::{FakeCodex, fixture, implementation_job};

#[test]
fn busy_deferred_footprint_holds_the_run_and_requeues_the_task() {
    if !super::dispatch_admission::isolated(
        "footprint_hold::busy_deferred_footprint_holds_the_run_and_requeues_the_task",
    ) {
        return;
    }
    let fx = fixture(
        "[workflow]\ndefault_crew = \"fixture\"\n\n\
         [crews.fixture]\nprovider = \"codex\"\nmodel = \"fixture-model\"\n",
    );
    // The fake editor proves it started by touching a marker; a held run
    // must never reach it.
    let marker = fx.repo.join("editor-started");
    let frame = json!({"type": "turn.failed", "error": {"message": "editor ran"}});
    let cli = FakeCodex::named(&fx.repo, "codex", &frame.to_string());
    std::fs::write(
        &cli.path,
        format!(
            "#!/bin/sh\ntouch '{}'\ncat > /dev/null\ncat <<'STDOUT'\n{frame}\nSTDOUT\nexit 1\n",
            marker.display()
        ),
    )
    .unwrap();
    let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let mut executor: ExecutorResource = serde_yaml::from_str(
        &std::fs::read_to_string(assets.join("executors/codex.yaml")).unwrap(),
    )
    .unwrap();
    executor.spec.command = Some(cli.path.display().to_string());
    executor.spec.sandbox = Some(ExecutorSandboxKind::Off);
    fx.runtime
        .upsert_executor_def(&ExecutorDef::from_resource_spec(
            executor.metadata.name,
            executor.spec,
            None,
            None,
        ))
        .unwrap();

    let jobs_dir = fx.runtime.global_root().join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).unwrap();
    let mut job = implementation_job(Provider::Codex);
    job.steps.remove(0);
    job.steps[0].recovery_activity = None;
    job.steps[0].resolved_recovery_activity = None;
    job.failure_activity = None;
    job.resolved_failure_activity = None;
    job.final_recovery_activity = None;
    job.resolved_final_recovery_activity = None;
    std::fs::write(jobs_dir.join("task_pr_pipeline.yaml"), serde_json::to_string(&json!({
        "schemaVersion": 2, "kind": "Job", "metadata": {"name": "task_pr_pipeline"}, "spec": job,
    })).unwrap()).unwrap();

    // Both tasks claim `file:feature.txt`; the holder in review keeps it.
    let task = fx.task("fixture");
    let holder = fx.task("fixture");
    fx.runtime
        .apply_task_automation_update(
            &holder,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                ..Default::default()
            },
        )
        .unwrap();

    let input = json!({
        "task_ids": [task],
        "crew": "fixture",
        "footprint_deferred": true,
        "footprint_wait_seconds": 0,
    });
    let run = fx
        .runtime
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), Some(input.clone()), None)
        .unwrap();
    fx.runtime
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id.clone(), input),
        )
        .unwrap();
    fx.runtime
        .apply_task_automation_update(
            &task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::InProgress),
                job_run_id: Some(run.run_id.clone()),
                ..Default::default()
            },
        )
        .unwrap();

    let _ = fx.runtime.execute_pipeline_run_worker(&run.run_id);
    let stored = fx.runtime.show_job_run(&run.run_id).unwrap();
    assert_eq!(stored.state, JobRunState::Held, "{:?}", stored.steps);
    let diagnostic = stored.steps.last().expect("run diagnostic");
    assert_eq!(
        diagnostic.error_code.as_deref(),
        Some(FOOTPRINT_UNAVAILABLE_ERROR_CODE)
    );
    assert!(
        diagnostic
            .error_message
            .as_deref()
            .unwrap()
            .contains(&holder),
        "the held diagnostic names the holder: {diagnostic:?}"
    );
    assert!(!marker.exists(), "no repair editor started");
    assert_eq!(fx.status(&task), TaskStatus::Backlog);

    let deferred = fx
        .runtime
        .audit_failure_incidents(&orbit_store::contracts::FailureIncidentQuery::default())
        .unwrap()
        .incidents
        .into_iter()
        .filter(|incident| {
            incident.surface == orbit_store::contracts::RUN_DEFERRED_SURFACE
                && incident.run_ids.contains(&run.run_id)
        })
        .map(|incident| incident.class)
        .collect::<Vec<_>>();
    assert_eq!(deferred, [orbit_store::contracts::FailureClass::Deferred]);

    let replay_count = fx.runtime.get_task_history(&task).unwrap().len();
    fx.runtime
        .finalize_job_run(&run.run_id, JobRunState::Held, Utc::now(), Some(1))
        .unwrap();
    assert_eq!(
        fx.runtime.get_task_history(&task).unwrap().len(),
        replay_count,
        "held cleanup is idempotent"
    );

    // Once the holder releases, a later admission takes the footprint.
    fx.runtime
        .apply_task_automation_update(
            &holder,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
        )
        .unwrap();
    let next = fx
        .runtime
        .insert_job_run(
            "task_pr_pipeline",
            1,
            Utc::now(),
            Some(json!({
                "task_ids": [task],
                "footprint_deferred": true,
                "footprint_wait_seconds": 0,
            })),
            None,
        )
        .unwrap();
    RuntimeHost::acquire_deferred_footprint(&fx.runtime, &next.run_id, "agent_review_repair")
        .expect("the footprint is taken once the holder releases it");
}

#[test]
fn stopped_run_waiting_for_its_footprint_is_not_held() {
    if !super::dispatch_admission::isolated(
        "footprint_hold::stopped_run_waiting_for_its_footprint_is_not_held",
    ) {
        return;
    }
    let fx = fixture(
        "[workflow]\ndefault_crew = \"fixture\"\n\n\
         [crews.fixture]\nprovider = \"codex\"\nmodel = \"fixture-model\"\n",
    );
    let task = fx.task("fixture");
    let holder = fx.task("fixture");
    fx.runtime
        .apply_task_automation_update(
            &holder,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                ..Default::default()
            },
        )
        .unwrap();
    // The run is not `Running`, so the wait ends on the stopped-run check
    // before any sleep, even with a long wait bound.
    let run = fx
        .runtime
        .insert_job_run(
            "task_pr_pipeline",
            1,
            Utc::now(),
            Some(json!({
                "task_ids": [task],
                "footprint_deferred": true,
                "footprint_wait_seconds": 3600,
            })),
            None,
        )
        .unwrap();
    let error =
        RuntimeHost::acquire_deferred_footprint(&fx.runtime, &run.run_id, "agent_review_repair")
            .unwrap_err();
    assert!(
        matches!(error, DispatchError::CliInvocationPermanent(_)),
        "a stopped run keeps the permanent failure, got: {error}"
    );
    assert!(
        error.to_string().contains("run stopped"),
        "the refusal names the stop: {error}"
    );
    assert_eq!(
        HeldFailure::of_local(Some(error.failure_code()), Some(&error.to_string())),
        None,
        "cancellation is never classified as a hold"
    );
}
