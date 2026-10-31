//! Provider failures through the public worker and its real run finalizer.
//! A scripted CLI makes capacity, usage limits and the negative control
//! deterministic without consuming provider quota or relying on host load.

use chrono::{Duration, Utc};
use orbit_core::TaskStatus;
use orbit_core::application::job::{JobRunListParams, job_run_to_json};
use orbit_core::metrics::reliability::ReliabilityWindow;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::resource::ExecutorResource;
use orbit_types::workflow::activity_job::Provider;
use orbit_types::workflow::{
    ExecutorDef, ExecutorSandboxKind, JobRunState, PipelineState, ProviderFailureClass,
};
use serde_json::json;

use super::provider_failure_hold::{FakeCodex, fixture, implementation_job};

fn local_provider_outcome(detail: &str, class: Option<ProviderFailureClass>) {
    let fx = fixture(
        "[workflow]\ndefault_crew = \"fixture\"\n\n\
         [crews.fixture]\nprovider = \"codex\"\nmodel = \"fixture-model\"\n",
    );
    let frame = json!({"type": "turn.failed", "error": {"message": detail}});
    let cli = FakeCodex::named(&fx.repo, "codex", &frame.to_string());
    if class.is_none() {
        std::fs::write(
            &cli.path,
            format!("#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{detail}' >&2\nexit 1\n"),
        )
        .unwrap();
    }
    let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let mut executor: ExecutorResource = serde_yaml::from_str(
        &std::fs::read_to_string(assets.join("executors/codex.yaml")).unwrap(),
    )
    .unwrap();
    executor.spec.command = Some(cli.path.display().to_string());
    // This fixture exercises finalization; OS confinement is covered separately.
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

    let task = fx.task("fixture");
    let input = json!({"task_ids": [task], "crew": "fixture"});
    let run = fx
        .runtime
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), Some(input.clone()), None)
        .unwrap();
    fx.runtime
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id, input),
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
    let expected = if class.is_some() {
        JobRunState::Held
    } else {
        JobRunState::Failed
    };
    assert_eq!(stored.state, expected, "{detail}: {:?}", stored.steps);
    let diagnostic = stored.steps.last().expect("run diagnostic");
    assert_eq!(
        diagnostic.error_code.as_deref(),
        class.map(|class| class.as_str())
    );
    assert!(
        diagnostic
            .error_message
            .as_deref()
            .unwrap()
            .contains(detail)
    );
    assert_eq!(
        fx.status(&task),
        if class.is_some() {
            TaskStatus::Backlog
        } else {
            TaskStatus::Blocked
        }
    );
    if let Some(class) = class {
        let hold = fx.last_hold(&task);
        assert_eq!(hold.class, class);
        assert_eq!(hold.excluded_crews, ["fixture"]);
        assert_eq!(hold.run_id, run.run_id);
        assert!(hold.not_before > stored.finished_at.unwrap());
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

        let waited = fx
            .runtime
            .wait_pipeline_runs(std::slice::from_ref(&run.run_id), 1, 1, None)
            .unwrap();
        assert_eq!(
            waited.results[0].status,
            orbit_core::PipelineWaitStatus::Held
        );
        assert!(waited.results[0].error.is_none());
        let parent_path = jobs_dir.join("provider_hold_parent.yaml");
        std::fs::write(&parent_path, serde_json::to_string(&json!({
            "schemaVersion": 2, "kind": "Job", "metadata": {"name": "provider_hold_parent"},
            "spec": {"state": "enabled", "steps": [{
                "id": "require_child_success", "spec": {"type": "deterministic", "action": "pipeline_success_guard", "config": {}},
                "default_input": {"result": "{{ input.result }}"},
            }]},
        })).unwrap()).unwrap();
        let mut child = serde_json::to_value(&waited.results[0]).unwrap();
        for _ in 0..2 {
            let parent = fx
                .runtime
                .run_job_v2_from_yaml(&parent_path, json!({"result": child}))
                .unwrap();
            assert!(parent.success, "{:?}", parent.message);
            assert_eq!(
                fx.runtime.show_job_run(&parent.run_id).unwrap().state,
                JobRunState::Success
            );
            child = json!({"status": "success", "run_id": parent.run_id});
        }
    }
    let history = fx
        .runtime
        .list_job_runs(JobRunListParams {
            state: Some(expected),
            ..Default::default()
        })
        .unwrap();
    assert!(history.iter().any(|row| row.run_id == run.run_id));
    let state = fx.runtime.read_run_state(&run.run_id).unwrap().unwrap();
    assert_eq!(
        job_run_to_json(&stored, Some(&state))["state"],
        expected.to_string()
    );
    let reliability = fx
        .runtime
        .pipeline_reliability(&ReliabilityWindow::ending_at(
            "fixture",
            Utc::now() + Duration::seconds(1),
            Duration::minutes(10),
        ))
        .unwrap();
    assert_eq!(
        reliability.job_runs.overall.held,
        u64::from(class.is_some())
    );
    assert_eq!(
        reliability.job_runs.overall.failed,
        u64::from(class.is_none())
    );
}

#[test]
fn local_capacity_is_held_with_its_task_and_parents() {
    if !super::dispatch_admission::isolated(
        "local_provider_holds::local_capacity_is_held_with_its_task_and_parents",
    ) {
        return;
    }
    local_provider_outcome(
        "Selected model is at capacity. Please try a different model.",
        Some(ProviderFailureClass::Capacity),
    );
}

#[test]
fn local_usage_limit_is_held_with_its_task_and_parents() {
    if !super::dispatch_admission::isolated(
        "local_provider_holds::local_usage_limit_is_held_with_its_task_and_parents",
    ) {
        return;
    }
    local_provider_outcome(
        "You've hit your usage limit.",
        Some(ProviderFailureClass::Limit),
    );
}

#[test]
fn an_unmarked_cli_failure_still_fails_the_local_run() {
    if !super::dispatch_admission::isolated(
        "local_provider_holds::an_unmarked_cli_failure_still_fails_the_local_run",
    ) {
        return;
    }
    local_provider_outcome("unrelated fixture stderr tail", None);
}

#[test]
fn declared_blocker_holds_the_local_run_and_passes_parent_guard() {
    if !super::dispatch_admission::isolated(
        "local_provider_holds::declared_blocker_holds_the_local_run_and_passes_parent_guard",
    ) {
        return;
    }
    let fx = fixture(
        "[workflow]\ndefault_crew = \"fixture\"\n\n[crews.fixture]\nprovider = \"codex\"\nmodel = \"fixture-model\"\n",
    );
    let output = json!({"schemaVersion": 1, "status": "success", "result": {"blocker": {"kind": "operator_approval_required", "evidence": "operator decision required"}}, "error": null});
    let cli = FakeCodex::named(&fx.repo, "codex", "");
    std::fs::write(
        &cli.path,
        format!("#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{}'\n", output),
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
    let jobs = fx.runtime.global_root().join("resources/jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    let mut job = implementation_job(Provider::Codex);
    job.steps.remove(0);
    job.steps[0].recovery_activity = None;
    job.steps[0].resolved_recovery_activity = None;
    job.failure_activity = None;
    job.resolved_failure_activity = None;
    job.final_recovery_activity = None;
    job.resolved_final_recovery_activity = None;
    std::fs::write(jobs.join("task_pr_pipeline.yaml"), json!({"schemaVersion": 2, "kind": "Job", "metadata": {"name": "task_pr_pipeline"}, "spec": job}).to_string()).unwrap();
    let task = fx.task("fixture");
    let input = json!({"task_ids": [task], "crew": "fixture"});
    let run = fx
        .runtime
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), Some(input.clone()), None)
        .unwrap();
    fx.runtime
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id, input),
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
    assert_eq!(fx.status(&task), TaskStatus::Blocked);
    let history = fx.runtime.get_task_history(&task).unwrap();
    assert!(
        history
            .last()
            .unwrap()
            .note
            .as_deref()
            .unwrap()
            .contains("error_code=task_blocked_by_agent"),
        "{history:?}"
    );
    assert_eq!(stored.state, JobRunState::Held, "{:?}", stored.steps);
    let diagnostic = stored.steps.last().unwrap();
    assert_eq!(
        diagnostic.error_code.as_deref(),
        Some("task_blocked_by_agent")
    );
    assert!(
        diagnostic
            .error_message
            .as_deref()
            .unwrap()
            .contains("kind=operator_approval_required")
    );
    let waited = fx
        .runtime
        .wait_pipeline_runs(std::slice::from_ref(&run.run_id), 1, 1, None)
        .unwrap();
    let parent = jobs.join("blocker_parent.yaml");
    std::fs::write(&parent, json!({"schemaVersion": 2, "kind": "Job", "metadata": {"name": "blocker_parent"}, "spec": {"state": "enabled", "steps": [{"id": "guard", "spec": {"type": "deterministic", "action": "pipeline_success_guard", "config": {}}, "default_input": {"result": "{{ input.result }}"}}]}}).to_string()).unwrap();
    let result = fx
        .runtime
        .run_job_v2_from_yaml(&parent, json!({"result": waited.results[0]}))
        .unwrap();
    assert!(result.success, "{:?}", result.message);

    let claimed = jobs.join("task_claimed_pr_pipeline.yaml");
    std::fs::write(&claimed, json!({"schemaVersion": 2, "kind": "Job", "metadata": {"name": "task_claimed_pr_pipeline"}, "spec": job}).to_string()).unwrap();
    let leaf = fx
        .runtime
        .run_job_v2_from_yaml(&claimed, json!({"crew": "fixture"}))
        .unwrap();
    let leaf_run = fx.runtime.show_job_run(&leaf.run_id).unwrap();
    assert_eq!(leaf_run.state, JobRunState::Held, "{:?}", leaf_run.steps);
    let waited = fx
        .runtime
        .wait_pipeline_runs(&[leaf.run_id], 1, 1, None)
        .unwrap();
    let parent_result = fx
        .runtime
        .run_job_v2_from_yaml(&parent, json!({"result": waited.results[0]}))
        .unwrap();
    assert!(parent_result.success, "{:?}", parent_result.message);

    for result in [
        json!({}),
        json!({"blocker": {"kind": "", "evidence": "invalid"}}),
    ] {
        let failed = json!({"schemaVersion": 1, "status": "failed", "result": result, "error": {"code": "example_failure", "message": "ordinary failure"}});
        std::fs::write(
            &cli.path,
            format!(
                "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{}'\nexit 1\n",
                failed
            ),
        )
        .unwrap();
        let outcome = fx
            .runtime
            .run_job_v2_from_yaml(&claimed, json!({"crew": "fixture"}))
            .unwrap();
        let stored = fx.runtime.show_job_run(&outcome.run_id).unwrap();
        assert_eq!(
            stored.state,
            JobRunState::Failed,
            "{failed}: {:?}",
            stored.steps
        );
    }
}
