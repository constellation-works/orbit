//! [ORB-20157] Jobs with `auto-task:` steps through the public runtime: a
//! disabled definition loads and explains with its targets resolved and
//! cannot be submitted, so no run is recorded and no task is minted; an
//! ill-typed one is refused at load. [ORB-20237] Such a job declares
//! `schemaVersion: 3`, which survives load and explain even when none of its
//! fields remain; any of the fields under `schemaVersion: 2`, even null, is
//! refused. [ORB-20158] An enabled one loads.

use orbit_core::application::job::JobRunListParams;
use orbit_core::{
    AutoTaskAddParams, AutoTaskSchedule, AutoTaskTemplate, DedupePolicy, OrbitRuntime,
    TaskPriority, TaskStatus, TaskType,
};
use serde_json::{Value, json};

const READINESS: &str = "\
schemaVersion: 3
kind: Job
metadata:
  name: release-readiness
spec:
  state: disabled
  expire_when: \"now >= job.started_at + duration('24h')\"
  steps:
    - id: qa
      target: auto-task:full-qa-sweep
      wait_until: board.empty
      require_result: release-readiness/v1
      reuse_when: \"result.status == 'passed' && result.revision == job.source_revision\"
    - id: security
      target: auto-task:security-review
      when: '{{ input.security }}'
      wait_until: \"self.prev.status == 'done' && board.empty\"
      expire_when: \"now >= self.wait_started_at + duration('8h')\"
";

/// An auto-task step nested in a parallel branch.
const NESTED_READINESS: &str = "\
schemaVersion: 3
kind: Job
metadata:
  name: release-readiness
spec:
  state: disabled
  expire_when: \"now >= job.started_at + duration('24h')\"
  steps:
    - id: checks
      parallel:
        join: { mode: all }
        branches:
          - id: qa
            target: auto-task:full-qa-sweep
            wait_until: board.empty
          - id: lint
            target: activity:agent_assess_diff
";

/// A schemaVersion 3 job with no auto-task field left.
const ORDINARY_V3: &str = "\
schemaVersion: 3
kind: Job
metadata:
  name: release-readiness
spec:
  state: disabled
  steps:
    - id: only
      target: activity:agent_assess_diff
";

fn runtime_with_job(name: &str, yaml: &str) -> OrbitRuntime {
    let runtime = OrbitRuntime::in_memory().unwrap();
    let jobs_dir = runtime.paths().jobs_dir.clone();
    std::fs::create_dir_all(&jobs_dir).unwrap();
    std::fs::write(jobs_dir.join(format!("{name}.yaml")), yaml).unwrap();
    runtime
}

fn add_auto_task(runtime: &OrbitRuntime, name: &str) {
    runtime
        .auto_task_add(AutoTaskAddParams {
            name: name.into(),
            description: "Full QA sweep".into(),
            schedule: AutoTaskSchedule::every_minutes(60),
            template: AutoTaskTemplate {
                title: "Full QA sweep".into(),
                description: "Sweep".into(),
                acceptance_criteria: vec!["Report findings".into()],
                task_type: TaskType::Chore,
                tags: Vec::new(),
                required_tools: Vec::new(),
                context_files: Vec::new(),
                priority: TaskPriority::Medium,
                complexity: None,
                crew: None,
                status: TaskStatus::Backlog,
            },
            dedupe: DedupePolicy::SkipIfOpen,
        })
        .unwrap();
}

fn a_disabled_auto_task_job_loads_explains_and_is_not_submitted() {
    let runtime = runtime_with_job("release-readiness", READINESS);
    add_auto_task(&runtime, "full-qa-sweep");
    let tasks_before = runtime.list_tasks().unwrap().len();

    let entry = runtime.show_job_catalog_entry("release-readiness").unwrap();
    assert_eq!(entry.spec.auto_task_steps().len(), 2);
    assert_eq!(
        orbit_core::application::job::job_catalog_to_json_with_last_run(&entry, None)["schema_version"],
        3
    );

    let report = runtime
        .run_tool(
            "orbit.automation.explain",
            json!({"definition": "job:release-readiness"}),
        )
        .unwrap();
    assert_eq!(report["definition"]["kind"], "job");
    assert_eq!(report["state"], "disabled");
    assert_eq!(report["schema_version"], 3);
    assert_eq!(report["expire_when"]["bound"], "after_start");
    assert_eq!(report["expire_when"]["after_start_seconds"], 24 * 3600);
    assert_eq!(report["pinned_revision"]["pinned"], true);
    assert_eq!(report["runtime"]["executable"], true);
    let steps = report["steps"].as_array().unwrap();
    let qa = &steps[0];
    assert_eq!(qa["target"]["auto_task"], "full-qa-sweep");
    assert_eq!(qa["target"]["resolution"], "resolved");
    assert_eq!(qa["target"]["admission_guard"]["kind"], "none");
    assert_eq!(
        qa["target"]["definition_digest"].as_str().map(str::len),
        Some(64),
        "the resolved definition is frozen by digest: {qa}"
    );
    assert_eq!(qa["require_result"], "release-readiness/v1");
    assert_eq!(qa["reads"]["board"], json!(["empty"]));
    assert_eq!(qa["status"], "not_evaluated");
    assert!(qa.get("prev").is_none(), "a first step has no self.prev");
    let security = &steps[1];
    assert_eq!(security["prev"], "qa");
    assert_eq!(security["when"], "{{ input.security }}");
    assert_eq!(
        security["target"]["resolution"], "missing",
        "an undefined target is reported, not resolved: {security}"
    );
    assert_eq!(
        security["expire_when"],
        "now >= self.wait_started_at + duration('8h')"
    );

    runtime
        .submit_job_run("release-readiness", json!({}), None)
        .expect_err("a disabled job must not be submitted");
    assert!(
        runtime
            .list_job_runs(JobRunListParams::default())
            .unwrap()
            .is_empty(),
        "a refused submission must record no run"
    );
    assert_eq!(
        runtime.list_tasks().unwrap().len(),
        tasks_before,
        "explain and a refused submission mint no task"
    );
}

/// Each case changes one thing in a specimen that loads.
fn ill_typed_auto_task_jobs_do_not_load() {
    for (case, yaml) in [
        ("nested", NESTED_READINESS.to_string()),
        (
            "enabled",
            READINESS.replace("state: disabled", "state: enabled"),
        ),
    ] {
        assert!(
            runtime_with_job("release-readiness", &yaml)
                .show_job_catalog_entry("release-readiness")
                .is_ok(),
            "{case}: the specimen loads under schemaVersion 3"
        );
    }
    for (case, yaml) in [
        (
            "forward reference",
            READINESS.replace(
                "wait_until: board.empty",
                "wait_until: \"steps['security'].status == 'done'\"",
            ),
        ),
        (
            "unbounded",
            READINESS.replace(
                "expire_when: \"now >= job.started_at + duration('24h')\"",
                "expire_when: \"now >= job.started_at + duration('200h')\"",
            ),
        ),
        (
            "unknown step key",
            READINESS.replace("require_result:", "require_results:"),
        ),
        (
            "auto-task fields hidden in schemaVersion 2",
            READINESS.replace("schemaVersion: 3", "schemaVersion: 2"),
        ),
        (
            "nested auto-task fields hidden in schemaVersion 2",
            NESTED_READINESS.replace("schemaVersion: 3", "schemaVersion: 2"),
        ),
        (
            "null step wait field in schemaVersion 2",
            ORDINARY_V3.replace("schemaVersion: 3", "schemaVersion: 2")
                + "      wait_until: null\n",
        ),
        (
            "null job expiry in schemaVersion 2",
            ORDINARY_V3
                .replace("schemaVersion: 3", "schemaVersion: 2")
                .replace("state: disabled", "state: disabled\n  expire_when: null"),
        ),
    ] {
        let runtime = runtime_with_job("release-readiness", &yaml);
        runtime
            .show_job_catalog_entry("release-readiness")
            .expect_err(case);
        let listed = runtime
            .list_job_catalog_with_last_run(
                true,
                orbit_core::application::job::JobCatalogFilter::All,
            )
            .unwrap();
        assert!(
            listed
                .iter()
                .all(|(entry, _)| entry.job_id != "release-readiness"),
            "{case}: a refused job must not be listed"
        );
        let explained = runtime
            .run_tool(
                "orbit.automation.explain",
                json!({"definition": "job:release-readiness"}),
            )
            .map(|value: Value| value.to_string());
        assert!(explained.is_err(), "{case}: explained a refused job");
    }
}

/// The declared version, not the remaining fields, is reported: a
/// schemaVersion 3 job whose fields are gone or null keeps it, and only an
/// explicit downgrade to 2 makes it ordinary.
fn a_schema_version_3_job_keeps_its_version_without_its_fields() {
    for (case, yaml) in [
        ("no auto-task field", ORDINARY_V3.to_string()),
        (
            "a null step wait field",
            ORDINARY_V3.to_string() + "      wait_until: null\n",
        ),
    ] {
        let runtime = runtime_with_job("release-readiness", &yaml);
        let entry = runtime
            .show_job_catalog_entry("release-readiness")
            .unwrap_or_else(|error| panic!("{case}: {error}"));
        assert_eq!(
            orbit_core::application::job::job_catalog_to_json_with_last_run(&entry, None)["schema_version"],
            3,
            "{case}"
        );
    }

    let runtime = runtime_with_job(
        "release-readiness",
        &ORDINARY_V3.replace("schemaVersion: 3", "schemaVersion: 2"),
    );
    let entry = runtime.show_job_catalog_entry("release-readiness").unwrap();
    assert!(!entry.spec.is_auto_task_job(), "the downgrade is ordinary");
    assert_eq!(
        orbit_core::application::job::job_catalog_to_json_with_last_run(&entry, None)["schema_version"],
        2
    );
}

// Exercise the owner's observation and engine settlement together, with the
// child already complete when the parked run resumes.
fn receipts_respect_step_expiry_after_downtime() {
    use chrono::{Duration, Utc};
    use orbit_core::application::task::TaskAddParams;
    use orbit_engine::{V2AuditWriter, execute_job_with_resume};
    use orbit_types::workflow::{
        AUTO_TASK_CHECKPOINT_VERSION, AutoTaskRunCheckpoint, AutoTaskStepRecord,
        AutoTaskStepStatus, PipelineState,
    };
    use std::sync::Arc;

    struct ReceiptHost<'a> {
        runtime: &'a OrbitRuntime,
        backdate_hours: i64,
    }
    impl orbit_engine::test_support::NullRuntimeHost for ReceiptHost<'_> {
        fn observe_auto_task_step(
            &self,
            run: &str,
            request: &orbit_engine::AutoTaskStepRequest,
        ) -> Result<orbit_engine::AutoTaskObservation, orbit_engine::DispatchError> {
            let mut observation =
                orbit_engine::RuntimeHost::observe_auto_task_step(self.runtime, run, request)?;
            if let Some(receipt) = &mut observation.receipt {
                receipt.settled_at -= Duration::hours(self.backdate_hours);
            }
            Ok(observation)
        }
        fn checkpoint_auto_task_step(
            &self,
            run: &str,
            step: &str,
            record: &AutoTaskStepRecord,
        ) -> Result<(), orbit_engine::DispatchError> {
            orbit_engine::RuntimeHost::checkpoint_auto_task_step(self.runtime, run, step, record)
        }
    }
    for (expiry, age_hours, backdate_hours, expected) in [
        (
            "now >= self.wait_started_at + duration('8h')",
            10,
            0,
            AutoTaskStepStatus::Timeout,
        ),
        (
            "now >= self.wait_started_at + duration('8h')",
            10,
            3,
            AutoTaskStepStatus::Done,
        ),
        ("board.empty", 10, 3, AutoTaskStepStatus::Timeout),
    ] {
        let runtime = OrbitRuntime::in_memory().unwrap();
        let child = runtime
            .add_task(TaskAddParams {
                title: "completed child".into(),
                status: Some(TaskStatus::Done),
                ..Default::default()
            })
            .unwrap();
        // The receipt's real timestamp fixes the relative clock without sleeps.
        let started = child.updated_at - Duration::hours(age_hours);
        let yaml = format!(
            "schemaVersion: 3\nkind: Job\nmetadata: {{ name: fixture }}\nspec:\n  state: enabled\n  expire_when: \"now >= job.started_at + duration('24h')\"\n  steps:\n    - id: qa\n      target: auto-task:full-qa-sweep\n      expire_when: \"{expiry}\"\n"
        );
        let job = orbit_engine::activity_job::load_job_asset(&yaml)
            .unwrap()
            .spec;
        let jobs = orbit_store::compose::workspace_job_run_store(
            runtime.sqlite_store().unwrap(),
            runtime.workspace_id().unwrap(),
        );
        let run = jobs
            .insert_job_run("fixture", 1, started, Some(json!({})), None)
            .unwrap();
        jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
            .unwrap();
        let mut state = PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({}));
        let record = AutoTaskStepRecord {
            auto_task: "full-qa-sweep".into(),
            activated_at: started,
            action_key: "fixture".into(),
            input_digest: "fixture".into(),
            status: AutoTaskStepStatus::Queued,
            reserved_at: Some(started),
            task_id: Some(child.id),
            minted: false,
            receipt: None,
            settled_at: None,
            diagnostic: None,
        };
        state.auto_task = Some(AutoTaskRunCheckpoint {
            version: AUTO_TASK_CHECKPOINT_VERSION,
            frozen_job: yaml,
            frozen_targets: Default::default(),
            started_at: started,
            source_revision: None,
            expires_at: started + Duration::hours(24),
            steps: [("qa".into(), record)].into(),
            branches: Default::default(),
            parked: None,
        });
        jobs.write_run_state(&run.run_id, &state).unwrap();
        let audit_dir =
            tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let audit = V2AuditWriter::with_disk_sinks(
            audit_dir.path(),
            Arc::new(orbit_store::Store::open_in_memory().unwrap()),
            "fixture",
            &run.run_id,
            "fixture",
            None,
        )
        .unwrap();
        let outcome = execute_job_with_resume(
            &job,
            json!({}),
            &run.run_id,
            audit,
            &ReceiptHost {
                runtime: &runtime,
                backdate_hours,
            },
            None,
        )
        .unwrap();
        let result = runtime
            .read_run_state(&run.run_id)
            .unwrap()
            .unwrap()
            .auto_task
            .unwrap();
        assert_eq!(
            result.steps["qa"].status, expected,
            "{expiry}, age {age_hours}: {outcome:?}"
        );
        assert_eq!(outcome.success, expected == AutoTaskStepStatus::Done);
    }
}

/// A later auto-task step waiting on an ordinary branch of an earlier
/// completed parallel block reads the branch's durable outcome: a succeeded
/// branch is `done`, a `when:`-skipped branch stays `skipped`, and a branch
/// checkpoint under a still-live block does not masquerade as done.
fn nested_parallel_branch_waits_observe_durable_outcomes() {
    use chrono::{Duration, Utc};
    use orbit_engine::{AutoTaskReadiness, AutoTaskStepRequest, RuntimeHost};
    use orbit_types::workflow::{
        AUTO_TASK_CHECKPOINT_VERSION, AutoTaskRunCheckpoint, JobRunState, PipelineState,
    };

    fn observe(
        lint_when: &str,
        expected_status: &str,
        branch_output: Value,
        checks_completed: bool,
    ) -> AutoTaskReadiness {
        let runtime = OrbitRuntime::in_memory().unwrap();
        add_auto_task(&runtime, "full-qa-sweep");
        let started = Utc::now() - Duration::hours(1);
        let yaml = format!(
            "schemaVersion: 3\nkind: Job\nmetadata: {{ name: nested-wait }}\nspec:\n  state: enabled\n  expire_when: \"now >= job.started_at + duration('24h')\"\n  steps:\n    - id: checks\n      parallel:\n        join: {{ mode: all }}\n        branches:\n          - id: lint\n            target: activity:agent_assess_diff\n{lint_when}    - id: qa\n      target: auto-task:full-qa-sweep\n      wait_until: \"steps['lint'].status == '{expected_status}'\"\n"
        );
        let jobs = orbit_store::compose::workspace_job_run_store(
            runtime.sqlite_store().unwrap(),
            runtime.workspace_id().unwrap(),
        );
        let run = jobs
            .insert_job_run("nested-wait", 1, started, Some(json!({})), None)
            .unwrap();
        jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
            .unwrap();
        let mut state = PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({}));
        if checks_completed {
            state.record_step(
                0,
                JobRunState::Success,
                Some(json!([{"branch_id": "lint", "outcome": "success"}])),
                None,
            );
        }
        state.auto_task = Some(AutoTaskRunCheckpoint {
            version: AUTO_TASK_CHECKPOINT_VERSION,
            frozen_job: yaml,
            frozen_targets: Default::default(),
            started_at: started,
            source_revision: None,
            expires_at: started + Duration::hours(24),
            steps: Default::default(),
            branches: [(
                "checks".to_string(),
                [("lint".to_string(), branch_output)].into(),
            )]
            .into(),
            parked: None,
        });
        jobs.write_run_state(&run.run_id, &state).unwrap();
        RuntimeHost::observe_auto_task_step(
            &runtime,
            &run.run_id,
            &AutoTaskStepRequest {
                step_id: "qa".into(),
                auto_task: "full-qa-sweep".into(),
            },
        )
        .unwrap()
        .readiness
    }

    assert_eq!(
        observe("", "done", json!({"lint": "ok"}), true),
        AutoTaskReadiness::Ready,
        "a succeeded ordinary branch of a completed parallel block is done"
    );
    assert_eq!(
        observe(
            "            when: \"{{{{ input.run_lint }}}}\"\n",
            "skipped",
            Value::Null,
            true,
        ),
        AutoTaskReadiness::Ready,
        "a skipped ordinary branch remains skipped, not done"
    );
    assert!(
        matches!(
            observe("", "done", json!({"lint": "ok"}), false),
            AutoTaskReadiness::Waiting(_)
        ),
        "a branch checkpoint under a still-live parallel block is not done"
    );
}

crate::scenario_table!(
    auto_task_job_steps_scenarios,
    nested_parallel_branch_waits_observe_durable_outcomes,
    receipts_respect_step_expiry_after_downtime,
    a_disabled_auto_task_job_loads_explains_and_is_not_submitted,
    ill_typed_auto_task_jobs_do_not_load,
    a_schema_version_3_job_keeps_its_version_without_its_fields
);
