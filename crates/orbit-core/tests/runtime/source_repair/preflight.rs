//! Lifecycle refusal must precede durable source-repair admission.
use super::*;
use orbit_core::application::task::TaskUpdateParams;

const PLACEHOLDER: &str = "To be authored by executing agent at start time.";

fn set_plan(f: &Fixture, plan: &str) {
    f.runtime
        .update_task_with_identity(
            &f.task,
            TaskUpdateParams {
                plan: Some(plan.into()),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
}

fn snapshot(f: &Fixture) -> Value {
    json!({"task":f.runtime.get_task(&f.task).unwrap(),
        "history":f.runtime.get_task_history(&f.task).unwrap(),
        "runs":f.runtime.job_history("task_pr_pipeline").unwrap(),
        "source":f.runtime.read_run_state(&f.source.run_id).unwrap(),
        "refs":git(&f.repo, &["ls-remote", "origin"])})
}

fn invalid_plans_refuse_before_any_admission_effect() {
    for status in [TaskStatus::Blocked, TaskStatus::Review] {
        for plan in ["", PLACEHOLDER] {
            let (f, _, _) = super::inline_publication::supported_incident();
            f.runtime
                .apply_task_automation_update(
                    &f.task,
                    TaskAutomationUpdate {
                        status: Some(status),
                        ..Default::default()
                    },
                )
                .unwrap();
            set_plan(&f, plan);
            let request = super::inline_publication::rework(&f);
            let before = snapshot(&f);
            let error = f
                .runtime
                .run_tool_as_human("orbit.workflow.run.resume", request)
                .unwrap_err();
            assert!(
                error.to_string().contains("non-empty execution plan"),
                "{status}, {plan:?}: {error}"
            );
            assert_eq!(
                snapshot(&f),
                before,
                "plan refusal must not reserve, charge, readmit, link, submit or change source"
            );
        }
    }
}

crate::scenario_table!(
    source_repair_preflight_scenarios,
    invalid_plans_refuse_before_any_admission_effect
);
