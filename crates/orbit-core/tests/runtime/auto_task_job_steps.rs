//! [ORB-20157] Jobs with `auto-task:` steps through the public runtime: a
//! disabled definition loads and explains with its targets resolved, an
//! enabled or ill-typed one is refused at load, and none can be submitted,
//! so no run is recorded and no task is minted.

use orbit_core::application::job::JobRunListParams;
use orbit_core::{
    AutoTaskAddParams, AutoTaskSchedule, AutoTaskTemplate, DedupePolicy, OrbitRuntime,
    TaskPriority, TaskStatus, TaskType,
};
use serde_json::{Value, json};

const READINESS: &str = "\
schemaVersion: 2
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
            schedule: AutoTaskSchedule::Interval { every_minutes: 60 },
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

    let report = runtime
        .run_tool(
            "orbit.automation.explain",
            json!({"definition": "job:release-readiness"}),
        )
        .unwrap();
    assert_eq!(report["definition"]["kind"], "job");
    assert_eq!(report["state"], "disabled");
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

    let refused = runtime
        .submit_job_run("release-readiness", json!({}), None)
        .unwrap_err()
        .to_string();
    assert!(refused.contains("auto-task steps"), "{refused}");
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

fn enabled_or_ill_typed_auto_task_jobs_do_not_load() {
    for (case, yaml, reason) in [
        (
            "enabled",
            READINESS.replace("state: disabled", "state: enabled"),
            "must stay `disabled`",
        ),
        (
            "forward reference",
            READINESS.replace(
                "wait_until: board.empty",
                "wait_until: \"steps['security'].status == 'done'\"",
            ),
            "comes after this step",
        ),
        (
            "unbounded",
            READINESS.replace(
                "expire_when: \"now >= job.started_at + duration('24h')\"",
                "expire_when: \"now >= job.started_at + duration('200h')\"",
            ),
            "the limit is 168h",
        ),
        (
            "unknown step key",
            READINESS.replace("require_result:", "require_results:"),
            "does not admit `require_results`",
        ),
    ] {
        let runtime = runtime_with_job("release-readiness", &yaml);
        let error = runtime
            .show_job_catalog_entry("release-readiness")
            .expect_err(case)
            .to_string();
        assert!(error.contains(reason), "{case}: {error}");
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

crate::scenario_table!(
    auto_task_job_steps_scenarios,
    a_disabled_auto_task_job_loads_explains_and_refuses_to_run,
    enabled_or_ill_typed_auto_task_jobs_do_not_load
);
