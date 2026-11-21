//! Provider failures through the public worker and its real run finalizer.
//! A scripted CLI makes capacity, usage limits and the negative control
//! deterministic without consuming provider quota or relying on host load.

use chrono::{Duration, Utc};
use orbit_core::application::job::{JobRunListParams, job_run_to_json};
use orbit_core::application::task::TaskUpdateParams;
use orbit_core::metrics::reliability::ReliabilityWindow;
use orbit_core::{OrbitRuntime, TaskStatus};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_tools::ToolContext;
use orbit_types::resource::ExecutorResource;
use orbit_types::workflow::activity_job::Provider;
use orbit_types::workflow::{
    ExecutorDef, ExecutorSandboxKind, JobRunState, PipelineState, ProviderFailureClass,
};
use serde_json::json;

use super::provider_failure_hold::{FakeCodex, fixture, implementation_job};

#[test]
fn muse_model_refusal_routes_the_requeued_task_around_its_recorded_pool_assignment() {
    const TEST: &str = "local_provider_holds::muse_model_refusal_routes_the_requeued_task_around_its_recorded_pool_assignment";
    if !super::dispatch_admission::isolated(TEST) {
        return;
    }
    if let Ok(run) = std::env::var("ORBIT_TEST_MUSE_MODEL_ACCESS_RUN") {
        let global = std::env::var_os("ORBIT_TEST_MUSE_MODEL_ACCESS_GLOBAL").unwrap();
        let workspace = std::env::var_os("ORBIT_TEST_MUSE_MODEL_ACCESS_WORKSPACE").unwrap();
        let runtime = OrbitRuntime::from_roots(global.as_ref(), workspace.as_ref()).unwrap();
        runtime.execute_pipeline_run_worker(&run).unwrap();
        return;
    }
    let fx = fixture(
        r#"[workflow]
default_crew = "wonder"
medium_complexity_crews = ["wonder@high"]
crew_selection = "static"

[crews.wonder]
provider = "muse"
model = "muse-wonder-full-1.0"

[crews.wonder-alias]
provider = "muse"
model = "muse-wonder-full-1.0"

[crews.spark]
provider = "muse"
model = "muse-spark-1.3"
"#,
    );
    let reason = "model `muse-wonder-full-1.0` does not exist or you lack access [request_id=12345678-1234-1234-1234-123456789abc]";
    let failed = json!({
        "payload_type": "run.terminal.completed",
        "payload": {"terminal": "failed", "text": reason, "status": 403}
    });
    let completed = json!({
        "payload_type": "run.terminal.completed",
        "payload": {"terminal": "completed", "text": json!({
            "schemaVersion": 1, "status": "success", "result": {"note": reason}, "error": null
        }).to_string()}
    });
    let cli = FakeCodex::named(&fx.repo, "muse", "");
    let launches = fx.repo.join("muse-launches");
    let recovered = fx.repo.join("muse-access-restored");
    std::fs::write(
        &cli.path,
        format!(
            "#!/bin/sh\nwhile [ \"$1\" != --model ]; do [ \"$#\" -gt 0 ] || exit 2; shift; done\n\
             printf '%s\\n' \"$2\" >> '{}'\n\
             if [ \"$2\" = muse-wonder-full-1.0 ] && [ ! -e '{}' ]; then printf '%s\\n' '{failed}'; exit 1; fi\n\
             printf '%s\\n' '{completed}'\n",
            launches.display(),
            recovered.display(),
        ),
    )
    .unwrap();
    let mut executor: ExecutorResource =
        serde_yaml::from_str(include_str!("../../assets/executors/muse.yaml")).unwrap();
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
    let mut job = implementation_job(Provider::Muse);
    job.steps.remove(0);
    job.steps[0].recovery_activity = None;
    job.steps[0].resolved_recovery_activity = None;
    job.failure_activity = None;
    job.resolved_failure_activity = None;
    job.final_recovery_activity = None;
    job.resolved_final_recovery_activity = None;
    let claimed_job_path = jobs.join("task_claimed_local_pipeline.yaml");
    std::fs::write(
        &claimed_job_path,
        json!({
            "schemaVersion": 2, "kind": "Job", "metadata": {"name": "task_claimed_local_pipeline"},
            "spec": job,
        })
        .to_string(),
    )
    .unwrap();
    let mut owner_job = json!(job);
    owner_job["steps"] = json!([{
        "id": "implement_bundle",
        "loop": {"items": "{{ input.task_ids }}", "max_iterations": 1, "steps": job.steps},
    }]);
    std::fs::write(
        jobs.join("task_local_pipeline.yaml"),
        json!({
            "schemaVersion": 2, "kind": "Job", "metadata": {"name": "task_local_pipeline"},
            "spec": owner_job,
        })
        .to_string(),
    )
    .unwrap();

    let task = fx.tagged_task(&[], None);
    assert_eq!(task.crew.as_deref(), Some("wonder@high"));
    assert_eq!(task.crew_source.unwrap().to_string(), "pool:medium");
    let input = json!({
        "task_ids": [task.id],
        "crew": "wonder@high",
        "medium_complexity_crews": ["wonder@high:1", "wonder-alias:3", "spark:1"],
        "crew_selection": {"task_id": task.id, "source": "pool:medium", "crew": "wonder@high"},
    });
    let run = fx
        .runtime
        .insert_job_run(
            "task_local_pipeline",
            1,
            Utc::now(),
            Some(input.clone()),
            None,
        )
        .unwrap();
    fx.runtime
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id, input),
        )
        .unwrap();
    fx.runtime
        .apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::InProgress),
                job_run_id: Some(run.run_id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    // Requeue waits for the recorded run owner to exit. Run this worker in
    // its own joined child so the fixture follows the same handoff.
    let mut worker = std::process::Command::new(std::env::current_exe().unwrap());
    worker
        .args(["--exact", TEST, "--nocapture"])
        .env("ORBIT_TEST_MUSE_MODEL_ACCESS_RUN", &run.run_id)
        .env(
            "ORBIT_TEST_MUSE_MODEL_ACCESS_GLOBAL",
            fx.runtime.global_root(),
        )
        .env(
            "ORBIT_TEST_MUSE_MODEL_ACCESS_WORKSPACE",
            fx.repo.join(".orbit"),
        );
    let output = orbit_common::test_env::run_child_test(&mut worker, TEST, &fx.repo);
    orbit_common::test_env::assert_child_test_passed(
        TEST,
        output.status,
        &output.stdout,
        &output.stderr,
    );
    let stored = fx.runtime.show_job_run(&run.run_id).unwrap();
    assert_eq!(stored.state, JobRunState::Failed, "{:?}", stored.steps);
    assert_eq!(
        stored.steps.last().unwrap().error_code.as_deref(),
        Some("muse_model_access_refused"),
    );
    let message = stored
        .steps
        .last()
        .unwrap()
        .error_message
        .as_deref()
        .unwrap();
    assert!(message.contains(reason), "{message}");
    assert!(!message.contains("muse login"), "{message}");
    fx.runtime
        .requeue_blocked_task_as(&task.id, "retry", false, Some("human:test"))
        .unwrap();

    let hold = fx.exclusion(&task.id, &[]);
    assert_eq!(hold["reason"], "provider_model_access_hold", "{hold}");
    let detail = hold["detail"].as_str().unwrap();
    assert!(detail.contains("muse-wonder-full-1.0"), "{detail}");
    assert!(detail.contains("successful invocation"), "{detail}");
    assert_eq!(
        std::fs::read_to_string(&launches).unwrap(),
        "muse-wonder-full-1.0\n"
    );

    let different_host =
        OrbitRuntime::from_roots(&fx.runtime.global_root(), &fx.repo.join(".orbit"))
            .unwrap()
            .with_automation_machine_identity(Some("hm_model_access_fixture".into()));
    let backlog = different_host
        .run_deterministic(
            "list_backlog_tasks",
            &json!({}),
            &json!({}),
            ToolContext::default(),
        )
        .unwrap();
    assert!(
        backlog["task_ids"]
            .as_array()
            .unwrap()
            .contains(&json!(task.id)),
        "{backlog}"
    );

    let replay = fx.runtime.replay_job_run(&run.run_id).unwrap();
    assert!(replay.success, "{replay:?}");
    let admitted = fx
        .runtime
        .show_job_run(&replay.run_id)
        .unwrap()
        .input
        .unwrap();
    assert_eq!(
        admitted["crew_selection"]["eligible_pool"],
        json!([{"name": "spark", "weight": 1}]),
    );
    assert_eq!(
        std::fs::read_to_string(&launches).unwrap(),
        "muse-wonder-full-1.0\nmuse-spark-1.3\n",
    );
    assert_eq!(
        fx.exclusion(&task.id, &[])["reason"],
        "provider_model_access_hold"
    );
    let replay = fx.runtime.replay_job_run(&run.run_id).unwrap();
    assert!(
        replay.success,
        "the completed answer cannot exclude Spark: {replay:?}"
    );

    // A deliberate task pin can verify access on the original account.
    fx.runtime
        .update_task_as_human(
            &task.id,
            TaskUpdateParams {
                crew: Some(Some("wonder@high".into())),
                ..Default::default()
            },
            "human:test".into(),
        )
        .unwrap();
    std::fs::write(&recovered, "").unwrap();
    let replay = fx.runtime.replay_job_run(&run.run_id).unwrap();
    assert!(replay.success, "{replay:?}");
    assert_eq!(
        std::fs::read_to_string(launches).unwrap(),
        "muse-wonder-full-1.0\nmuse-spark-1.3\nmuse-spark-1.3\nmuse-wonder-full-1.0\n",
    );
    fx.runtime
        .update_task_as_human(
            &task.id,
            TaskUpdateParams {
                crew: Some(None),
                ..Default::default()
            },
            "human:test".into(),
        )
        .unwrap();
    assert!(
        fx.admitted(&task.id, &[]),
        "the newer same-model success clears the hold"
    );

    // The claimed delivery has the same provider diagnostic and failed run
    // outcome. Even on this fixture's host, it is a separate routing surface.
    std::fs::remove_file(recovered).unwrap();
    let claimed = fx
        .runtime
        .run_job_v2_from_yaml(
            &claimed_job_path,
            json!({"task_ids": [task.id], "crew": "wonder@high"}),
        )
        .unwrap();
    let recorded = fx.runtime.show_job_run(&claimed.run_id).unwrap();
    assert_eq!(recorded.state, JobRunState::Failed);
    assert_eq!(
        recorded.steps.last().unwrap().error_code.as_deref(),
        Some("muse_model_access_refused"),
    );
    assert!(claimed.message.unwrap().contains(reason));
    assert!(
        fx.admitted(&task.id, &[]),
        "claimed evidence cannot hold local admission"
    );
}

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
fn local_authentication_failure_is_held_with_its_task_and_parents() {
    if !super::dispatch_admission::isolated(
        "local_provider_holds::local_authentication_failure_is_held_with_its_task_and_parents",
    ) {
        return;
    }
    local_provider_outcome(
        "OAuth session expired",
        Some(ProviderFailureClass::Unavailable),
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

#[test]
fn unresolved_policy_decision_holds_the_run_and_keeps_the_candidate() {
    if !super::dispatch_admission::isolated(
        "local_provider_holds::unresolved_policy_decision_holds_the_run_and_keeps_the_candidate",
    ) {
        return;
    }

    const CODE: &str = "unresolved_policy_decision";
    const EVIDENCE: &str = "The fix is prepared, but the required check remains red pending the unresolved operator decision";
    let fx = fixture(
        "[workflow]\ndefault_crew = \"fixture\"\n\n[crews.fixture]\nprovider = \"codex\"\nmodel = \"fixture-model\"\n",
    );
    let git = |repo: &std::path::Path, args: &[&str]| {
        let output = crate::git_test_support::run(repo, args).unwrap();
        assert!(output.success, "{args:?}: {}", output.stderr);
        output.stdout.trim().to_string()
    };
    git(&fx.repo, &["init", "-b", "main"]);
    git(
        &fx.repo,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "base",
        ],
    );
    let base = git(&fx.repo, &["rev-parse", "HEAD"]);
    let cli = FakeCodex::named(&fx.repo, "codex", "");
    let mut executor: ExecutorResource =
        serde_yaml::from_str(include_str!("../../assets/executors/codex.yaml")).unwrap();
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

    let resources = fx.runtime.global_root().join("resources");
    let activities = resources.join("activities");
    std::fs::create_dir_all(&activities).unwrap();
    for (name, content) in [
        (
            "worktree_setup.yaml",
            include_str!("../../assets/activities/worktree_setup.yaml"),
        ),
        (
            "pr_failure_handoff.yaml",
            include_str!("../../assets/activities/pr_failure_handoff.yaml"),
        ),
    ] {
        std::fs::write(activities.join(name), content).unwrap();
    }
    let jobs = resources.join("jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    let agent = orbit_types::workflow::activity_job::ActivityV2Spec::AgentLoop(
        orbit_types::workflow::activity_job::AgentLoopSpec::for_provider(
            "implement the task".to_string(),
            Provider::Codex,
            30,
        ),
    );
    let job = jobs.join("task_pr_pipeline.yaml");
    std::fs::write(
        &job,
        json!({
            "schemaVersion": 2, "kind": "Job", "metadata": {"name": "task_pr_pipeline"},
            "spec": {"state": "enabled", "kind": "workflow", "failure_activity": "pr_failure_handoff",
                "steps": [
                    {"id": "worktree", "target": "activity:worktree_setup",
                     "default_input": {"task_ids": "{{ input.task_ids }}", "base": "main", "base_sync": "local"}},
                    {"id": "implement_one", "spec": agent, "default_input": {
                        "workspace_path": "{{ steps.worktree.output.workspace_path }}",
                        "repo_root": "{{ steps.worktree.output.workspace_path }}"}}
                ]}
        })
        .to_string(),
    )
    .unwrap();

    let run = |reply: serde_json::Value, ending: &str| {
        std::fs::write(
            &cli.path,
            format!("#!/bin/sh\ncat > /dev/null\nprintf 'partial candidate\\n' > candidate.txt\nprintf '%s\\n' '{reply}'\n{ending}\n"),
        )
        .unwrap();
        let task = fx.task("fixture");
        let outcome = fx
            .runtime
            .run_job_v2_from_yaml(&job, json!({"task_ids": [task], "crew": "fixture"}))
            .unwrap();
        let stored = fx.runtime.show_job_run(&outcome.run_id).unwrap();
        (task, stored)
    };

    let error = |code| json!({"code": code, "message": EVIDENCE});
    let failed =
        |error| json!({"schemaVersion": 1, "status": "failed", "result": {}, "error": error});
    let mut incomplete = failed(error("implementation_incomplete"));
    incomplete["result"]["remaining_work"] = json!(["finish the repair"]);
    let mut timeout = failed(error(CODE));
    timeout["status"] = json!("timeout");
    for reply in [
        failed(json!({"code": CODE})),
        failed(json!({"code": CODE, "message": "  "})),
        failed(json!({"code": CODE, "message": 42})),
        failed(error("unresolved_policy_decision_error")),
        timeout,
        incomplete,
    ] {
        let (_, stored) = run(reply.clone(), "exit 0");
        assert_eq!(
            stored.state,
            JobRunState::Failed,
            "{reply}: {:?}",
            stored.steps
        );
        let message = stored
            .steps
            .last()
            .unwrap()
            .error_message
            .as_deref()
            .unwrap();
        assert!(
            message.contains("declared envelope status="),
            "{reply}: {message}"
        );
    }
    let (_, provider) = run(failed(error("provider_unavailable")), "exit 0");
    assert_eq!(provider.state, JobRunState::Held, "{:?}", provider.steps);
    assert_eq!(
        provider.steps.last().unwrap().error_code.as_deref(),
        Some("provider_unavailable")
    );

    let (_, nonzero) = run(failed(error(CODE)), "exit 1");
    assert_eq!(nonzero.state, JobRunState::Failed, "{:?}", nonzero.steps);
    let (_, auth_failure) = run(
        failed(error(CODE)),
        "echo 'OAuth session expired' >&2\nexit 1",
    );
    assert_eq!(
        auth_failure.state,
        JobRunState::Held,
        "{:?}",
        auth_failure.steps
    );
    assert_eq!(
        auth_failure.steps.last().unwrap().error_code.as_deref(),
        Some("provider_unavailable")
    );

    let (task, stored) = run(failed(error(CODE)), "exit 0");
    assert_eq!(stored.state, JobRunState::Held, "{:?}", stored.steps);
    assert_eq!(
        stored.steps.last().unwrap().error_code.as_deref(),
        Some("task_blocked_by_agent")
    );
    assert_eq!(fx.status(&task), TaskStatus::Blocked);
    let history = fx.runtime.get_task_history(&task).unwrap();
    assert!(
        history.iter().any(|entry| matches!(
            &entry.decision,
            Some(orbit_types::task::TaskDecision::AgentBlocked { blocker })
                if blocker.kind == CODE && blocker.evidence == EVIDENCE
        )),
        "{history:?}"
    );
    let state = fx.runtime.read_run_state(&stored.run_id).unwrap().unwrap();
    let handoff = state.failure_activity_checkpoint.as_ref().unwrap();
    assert_eq!(handoff.failed_step_id, "implement_one");
    assert_eq!(handoff.output["decision"], "blocked_by_agent");
    assert_eq!(handoff.output["pr_created"], false);
    let worktree = state.pipeline_step_output("worktree").unwrap()["workspace_path"]
        .as_str()
        .unwrap();
    let worktree = std::path::Path::new(worktree);
    assert_eq!(git(worktree, &["rev-parse", "HEAD"]), base);
    assert_eq!(
        git(worktree, &["status", "--short", "--", "candidate.txt"]),
        "?? candidate.txt"
    );
    assert_eq!(
        std::fs::read_to_string(worktree.join("candidate.txt")).unwrap(),
        "partial candidate\n"
    );
}
