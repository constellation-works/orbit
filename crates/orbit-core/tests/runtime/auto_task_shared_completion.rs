//! Compatible jobs share a child without sharing their durable obligations.

use chrono::{Duration, Utc};
use orbit_core::OrbitRuntime;
use orbit_core::application::task::TaskUpdateParams;
use orbit_core::runtime::scripts::ScriptGrantOptions;
use orbit_engine::{
    AutoTaskAdmission, AutoTaskStepRequest, RuntimeHost, ScriptStepEnd, ScriptStepOutputs,
    ScriptStepRan, ScriptStepRun,
};
use orbit_types::task::{TaskArtifact, TaskStatus};
use orbit_types::workflow::activity_job::ScriptExecutionRecord;
use orbit_types::workflow::{
    AUTO_TASK_CHECKPOINT_VERSION, AutoTaskCompletionContract, AutoTaskRunCheckpoint,
    AutoTaskStepRecord, AutoTaskStepStatus, JobRunState, PipelineState,
};
use serde_json::json;

const TARGET: &str = "full-qa-sweep";

fn reserve(runtime: &OrbitRuntime, name: &str, revision: &str) -> (String, AutoTaskStepRecord) {
    let now = Utc::now();
    let yaml = format!(
        "schemaVersion: 3\nkind: Job\nmetadata: {{ name: {name} }}\nspec:\n  state: enabled\n  expire_when: \"now >= job.started_at + duration('24h')\"\n  steps:\n    - id: qa\n      target: auto-task:{TARGET}\n      require_result: release-readiness/v1\n"
    );
    let jobs = orbit_store::compose::workspace_job_run_store(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    );
    let run = jobs
        .insert_job_run(name, 1, now, Some(json!({})), None)
        .unwrap();
    jobs.mark_job_run_running(&run.run_id, now, std::process::id())
        .unwrap();
    let mut state = PipelineState::new(run.run_id.clone(), name.into(), json!({}));
    state.auto_task = Some(AutoTaskRunCheckpoint {
        version: AUTO_TASK_CHECKPOINT_VERSION,
        frozen_job: yaml,
        frozen_targets: Default::default(),
        started_at: now,
        source_revision: Some(revision.into()),
        expires_at: now + Duration::hours(24),
        steps: Default::default(),
        branches: Default::default(),
        parked: None,
    });
    jobs.write_run_state(&run.run_id, &state).unwrap();
    let observation = observe(runtime, &run.run_id);
    assert_eq!(observation.definition_hold, None);
    let record = AutoTaskStepRecord {
        auto_task: TARGET.into(),
        activated_at: now,
        action_key: observation.action_key,
        input_digest: observation.input_digest,
        status: AutoTaskStepStatus::Waiting,
        reserved_at: Some(now),
        task_id: None,
        minted: false,
        receipt: None,
        settled_at: None,
        diagnostic: None,
    };
    RuntimeHost::checkpoint_auto_task_step(runtime, &run.run_id, "qa", &record).unwrap();
    (run.run_id, record)
}

fn observe(runtime: &OrbitRuntime, run: &str) -> orbit_engine::AutoTaskObservation {
    RuntimeHost::observe_auto_task_step(
        runtime,
        run,
        &AutoTaskStepRequest {
            step_id: "qa".into(),
            auto_task: TARGET.into(),
        },
    )
    .unwrap()
}

fn compatible_jobs_accept_one_child_and_reject_other_inputs() {
    let runtime = OrbitRuntime::in_memory().unwrap();
    let repo = &runtime.paths().repo_root;
    let git = |args: &[&str]| {
        let output = crate::git_test_support::run(repo, args).unwrap();
        assert!(output.success, "git {args:?}: {}", output.stderr);
        output.stdout.trim().to_string()
    };
    git(&["init", "-q"]);
    let branch = format!("refs/heads/{}", runtime.workflow_base_branch());
    git(&["symbolic-ref", "HEAD", &branch]);
    git(&[
        "-c",
        "user.name=qa",
        "-c",
        "user.email=qa@example.com",
        "commit",
        "--allow-empty",
        "-qm",
        "Fixture revision",
    ]);
    let revision = git(&["rev-parse", "HEAD"]);

    super::add_auto_task(&runtime, TARGET);
    let mut definition = runtime.auto_task_show(TARGET).unwrap().unwrap();
    definition.completion = Some(AutoTaskCompletionContract {
        contract: "release-readiness/v1".into(),
        adapter: "accept-evidence".into(),
        form: "fixture/v1".into(),
        evidence_artifact: "evidence.json".into(),
        max_age_minutes: 60,
        read_task_tags: Vec::new(),
        evidence_instructions: None,
    });
    let definition_path =
        orbit_automation::auto_tasks::loader::definition_path(&runtime.paths().local_dir, TARGET);
    let definition_yaml = serde_yaml::to_string(&definition).unwrap();
    std::fs::write(&definition_path, &definition_yaml).unwrap();
    let scripts = repo.join(".orbit/resources/scripts");
    std::fs::create_dir_all(&scripts).unwrap();
    std::fs::write(
        scripts.join("accept-evidence.yaml"),
        "schemaVersion: 1\nkind: Script\nmetadata: { name: accept-evidence }\nspec:\n  entry: accept-evidence.py\n  interpreter: [python3]\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("accept-evidence.py"),
        "import json, sys\nv = json.load(sys.stdin)\nprint(json.dumps(dict(schema_version=1, contract=v['contract'], form=v['form'], task_id=v['trusted']['task_id'], source_revision=v['trusted']['source_revision'], status='passed')))\n",
    )
    .unwrap();
    runtime
        .grant_script("accept-evidence", &ScriptGrantOptions::default())
        .unwrap();

    let mut consumers = Vec::new();
    for (name, expected_minted) in [("first-readiness", true), ("second-readiness", false)] {
        let (run, mut record) = reserve(&runtime, name, &revision);
        let AutoTaskAdmission::Bound { task_id, minted } =
            RuntimeHost::admit_auto_task_step(&runtime, &run, "qa").unwrap()
        else {
            panic!("compatible job {name} must bind")
        };
        assert_eq!(minted, expected_minted);
        record.task_id = Some(task_id);
        record.minted = minted;
        record.status = AutoTaskStepStatus::Queued;
        RuntimeHost::checkpoint_auto_task_step(&runtime, &run, "qa", &record).unwrap();
        consumers.push((run, record));
    }
    assert_ne!(consumers[0].1.action_key, consumers[1].1.action_key);
    assert_eq!(consumers[0].1.input_digest, consumers[1].1.input_digest);
    assert_eq!(consumers[0].1.task_id, consumers[1].1.task_id);
    let child = consumers[0].1.task_id.clone().unwrap();

    // Different definitions and current source revisions cannot bind this open child.
    definition.description.push_str(" changed");
    std::fs::write(
        &definition_path,
        serde_yaml::to_string(&definition).unwrap(),
    )
    .unwrap();
    let (different_definition, _) = reserve(&runtime, "different-definition", &revision);
    assert!(matches!(
        RuntimeHost::admit_auto_task_step(&runtime, &different_definition, "qa").unwrap(),
        AutoTaskAdmission::Held(_)
    ));
    std::fs::write(&definition_path, definition_yaml).unwrap();
    git(&[
        "-c",
        "user.name=qa",
        "-c",
        "user.email=qa@example.com",
        "commit",
        "--allow-empty",
        "-qm",
        "Next fixture revision",
    ]);
    let next_revision = git(&["rev-parse", "HEAD"]);
    let (different_source, _) = reserve(&runtime, "different-source", &next_revision);
    assert!(matches!(
        RuntimeHost::admit_auto_task_step(&runtime, &different_source, "qa").unwrap(),
        AutoTaskAdmission::Held(_)
    ));
    git(&["update-ref", &branch, &revision]);
    assert_eq!(runtime.list_tasks().unwrap().len(), 1);

    let jobs = orbit_store::compose::workspace_job_run_store(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    );
    let execution = jobs
        .insert_job_run(
            "execute-child",
            1,
            Utc::now(),
            Some(json!({"task_id": child})),
            None,
        )
        .unwrap();
    jobs.mark_job_run_running(&execution.run_id, Utc::now(), std::process::id())
        .unwrap();
    jobs.finalize_job_run(&execution.run_id, JobRunState::Success, Utc::now(), None)
        .unwrap();
    let state = runtime.read_run_state(&consumers[0].0).unwrap().unwrap();
    let digest = &state.auto_task.unwrap().frozen_targets[TARGET].digest;
    let report = "{\"findings\": []}";
    let candidate = json!({
        "schema_version": 1,
        "contract": "release-readiness/v1",
        "binding": {
            "workspace_id": runtime.workspace_id().unwrap(),
            "definition": TARGET,
            "definition_digest": digest,
            "task_id": child,
            "run_id": execution.run_id,
            "source_revision": revision,
        },
        "artifacts": [{"path": "report.json", "sha256": orbit_common::security::release::sha256_hex(report.as_bytes())}],
    });
    runtime
        .force_update_task_with_identity(
            &child,
            TaskUpdateParams {
                job_run_id: Some(Some(execution.run_id.clone())),
                upsert_artifacts: vec![
                    TaskArtifact::from_text("report.json", report),
                    TaskArtifact::from_text("evidence.json", candidate.to_string()),
                ],
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    runtime
        .force_update_task_with_identity(
            &child,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();

    let adapter_digest = runtime
        .script_status("accept-evidence")
        .unwrap()
        .unwrap()
        .grant
        .unwrap()
        .digest;
    orbit_core::test_support::with_completion_adapter_run(
        move |request| {
            assert_eq!(request.script, "accept-evidence");
            let input = &request.input;
            ScriptStepRun::Ran(Box::new(ScriptStepRan {
                end: ScriptStepEnd::Exited(0),
                stdout: serde_json::to_vec(&json!({
                    "schema_version": 1,
                    "contract": input["contract"],
                    "form": input["form"],
                    "task_id": input["trusted"]["task_id"],
                    "source_revision": input["trusted"]["source_revision"],
                    "status": "passed",
                }))
                .unwrap(),
                stdout_over_cap: false,
                output_schema: serde_json::Value::Null,
                log: Vec::new(),
                record: ScriptExecutionRecord {
                    digest: adapter_digest.clone(),
                    ..Default::default()
                },
                outputs: ScriptStepOutputs::None,
            }))
        },
        || {
            // Replaying a compatible digest into a binding still cannot override
            // a consumer's different frozen definition or pinned source.
            for run in [&different_definition, &different_source] {
                let mut record = observe(&runtime, run).record.unwrap();
                record.task_id = Some(child.clone());
                record.input_digest = consumers[0].1.input_digest.clone();
                RuntimeHost::checkpoint_auto_task_step(&runtime, run, "qa", &record).unwrap();
                let observation = observe(&runtime, run);
                assert!(observation.result_hold.is_some(), "{run}: {observation:?}");
                assert!(observation.receipt.unwrap().result.is_none());
            }
            for (run, mut record) in consumers {
                let observation = observe(&runtime, &run);
                assert_eq!(observation.definition_hold, None);
                assert_eq!(observation.result_hold, None, "{run}: {observation:?}");
                let receipt = observation.receipt.unwrap();
                let result = receipt
                    .result
                    .as_ref()
                    .expect("each consumer accepts the shared completion");
                assert_eq!(result.task_id, child);
                assert_eq!(result.run_id, execution.run_id);
                assert_eq!(result.revision, revision);
                assert_eq!(
                    result.status,
                    orbit_types::workflow::AutoTaskResultStatus::Passed
                );
                record.receipt = Some(receipt);
                record.status = AutoTaskStepStatus::Done;
                record.settled_at = Some(Utc::now());
                RuntimeHost::checkpoint_auto_task_step(&runtime, &run, "qa", &record).unwrap();
            }
        },
    );
    assert_eq!(
        runtime.list_tasks().unwrap().len(),
        1,
        "acceptance never remints"
    );
}

crate::scenario_table!(
    shared_completion_scenarios,
    compatible_jobs_accept_one_child_and_reject_other_inputs
);
