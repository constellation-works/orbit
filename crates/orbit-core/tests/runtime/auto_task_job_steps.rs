//! [ORB-20157] Jobs with `auto-task:` steps through the public runtime: a
//! disabled definition loads and explains with its targets resolved, an
//! enabled or ill-typed one is refused at load, and none can be submitted,
//! so no run is recorded and no task is minted. [ORB-20237] Such a job
//! declares `schemaVersion: 3`, which survives load and explain and keeps
//! the job phase-1 only even when none of its fields remain; any of the
//! fields under `schemaVersion: 2`, even null, is refused.

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

fn a_disabled_auto_task_job_loads_explains_and_refuses_to_run() {
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
    assert_eq!(report["runtime"]["executable"], false);
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
        .expect_err("a job with auto-task steps must not be submitted");
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
fn enabled_or_ill_typed_auto_task_jobs_do_not_load() {
    assert!(
        runtime_with_job("release-readiness", NESTED_READINESS)
            .show_job_catalog_entry("release-readiness")
            .is_ok(),
        "the nested specimen loads under schemaVersion 3"
    );
    for (case, yaml) in [
        (
            "enabled",
            READINESS.replace("state: disabled", "state: enabled"),
        ),
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
            "enabled schemaVersion 3 without auto-task fields",
            ORDINARY_V3.replace("state: disabled", "state: enabled"),
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

/// The declared version, not the remaining fields, decides phase 1: a
/// schemaVersion 3 job whose fields are gone or null still cannot be
/// submitted, and only an explicit downgrade to 2 makes it ordinary.
fn a_schema_version_3_job_stays_phase_one_without_its_fields() {
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
        runtime
            .submit_job_run("release-readiness", json!({}), None)
            .expect_err(case);
        assert!(
            runtime
                .list_job_runs(JobRunListParams::default())
                .unwrap()
                .is_empty(),
            "{case}: a refused submission must record no run"
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

crate::scenario_table!(
    auto_task_job_steps_scenarios,
    a_disabled_auto_task_job_loads_explains_and_refuses_to_run,
    enabled_or_ill_typed_auto_task_jobs_do_not_load,
    a_schema_version_3_job_stays_phase_one_without_its_fields
);
