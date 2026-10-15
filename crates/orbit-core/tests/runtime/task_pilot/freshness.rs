//! Completed explicit pilot evidence is reusable by a matching routine, while
//! active work and changes to the consumer's material contract remain guarded.

use orbit_core::application::task::TaskUpdateParams;
use orbit_types::workflow::automation::members::SourceSensitivity;

use super::races::apply_input;
use super::*;

fn assess(workspace: &Workspace, task: &Task, warning: bool) {
    let prepared = workspace.prepare(&[&task.id]);
    assert_eq!(prepared["task_ids"], json!([task.id]));
    let mut input = apply_input(workspace, &prepared);
    if warning {
        input["results"][0]["tasks"][0]["utility_warnings"] =
            json!(["Operator review is needed before implementation."]);
    }
    let applied = workspace.action("apply_task_pilot_results", input);
    assert_eq!(applied["applied_count"], 1, "{applied}");
    assert_eq!(applied["status"], "succeeded", "{applied}");
}

fn assert_fresh(workspace: &Workspace, routine: &RoutineDefinition, reason: &str) {
    // Both real evaluations exceed the debounce/max-wait windows. Fresh
    // evidence must never reach provider admission, even on the next tick.
    for minutes in [3, 15] {
        let evaluated = evaluate_routine(
            &workspace.runtime,
            routine,
            false,
            Utc::now() + Duration::minutes(minutes),
        )
        .unwrap();
        assert_eq!(evaluated.reason, reason, "{evaluated:?}");
        assert!(evaluated.batch.is_empty());
        let members = evaluated.state.unwrap().members.unwrap();
        assert!(members.pending.is_empty());
        assert!(members.active.is_none());
        assert!(members.withheld.is_empty());
    }
    assert!(
        workspace
            .jobs
            .list_job_runs("task_pilot_pipeline")
            .unwrap()
            .is_empty()
    );
}

fn assert_preparation_due(workspace: &Workspace, routine: &RoutineDefinition, task: &Task) {
    let now = Utc::now();
    let observed = evaluate_routine(&workspace.runtime, routine, false, now).unwrap();
    assert_eq!(observed.reason, "debouncing", "{observed:?}");
    assert!(
        observed
            .state
            .unwrap()
            .members
            .unwrap()
            .pending
            .contains_key(&task.id)
    );
    let due = evaluate_routine(
        &workspace.runtime,
        routine,
        true,
        now + Duration::minutes(3),
    )
    .unwrap();
    assert_eq!(due.reason, "would_fire", "{due:?}");
    assert_eq!(due.batch.len(), 1);
    assert_eq!(due.batch[0].task_ids, [task.id.clone()]);
}

fn explicit_assessment_survives_selector_apply_promotion_and_administrative_edits() {
    let workspace = Workspace::new();
    let task = workspace.task("fresh explicit assessment");
    assert!(task.context_files.is_empty());
    assess(&workspace, &task, false);
    let stored = workspace.runtime.get_task(&task.id).unwrap();
    assert_eq!(stored.context_files, ["file:README.md"]);
    assert_eq!(stored.status, TaskStatus::Proposed);
    workspace
        .runtime
        .update_task_as_human(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
    let routine = pilot_routine();
    assert_fresh(&workspace, &routine, "fresh");
    workspace
        .runtime
        .run_tool(
            "orbit.task.update",
            json!({
                "id": task.id, "model": "codex", "comment": "Administrative observation.",
                "priority": "high", "execution_summary": "Still awaiting ordinary dispatch.",
            }),
        )
        .unwrap();
    workspace.commit_file(
        "unrelated.rs",
        "fn unrelated() {}\n",
        "unrelated source move",
    );
    assert_fresh(&workspace, &routine, "fresh");

    let readiness = workspace
        .runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    let entry = readiness["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == task.id)
        .unwrap();
    assert_eq!(entry["eligible"], true, "{readiness}");
    let selected = workspace.action("list_backlog_tasks", json!({}));
    assert_eq!(selected["task_ids"], json!([task.id]), "{selected}");
    // An explicit deliberate audit still selects this task despite freshness.
    assert_eq!(workspace.prepare(&[&task.id])["task_ids"], json!([task.id]));

    workspace
        .runtime
        .run_tool(
            "orbit.task.update",
            json!({
                "id": task.id, "model": "codex", "description": "Materially changed requirements.",
            }),
        )
        .unwrap();
    assert_preparation_due(&workspace, &routine, &task);
}

fn routine_reuse_obeys_configured_material_source_and_consumer_policy() {
    for change in ["material", "source", "policy", "consumer"] {
        let mut workspace = Workspace::new();
        let config = workspace.repo.join(".orbit/config.toml");
        let mut contents = std::fs::read_to_string(&config).unwrap();
        contents.push_str("\n[workflow.task_pilot_freshness]\nmaterial_fields = [\"title\", \"selectors\", \"tags\"]\nsource_sensitivity = \"context_files\"\n");
        std::fs::write(config, contents).unwrap();
        workspace.runtime = runtime_at(
            &workspace.runtime.global_root(),
            &workspace.repo.join(".orbit"),
        );
        let task = workspace.task(change);
        assess(&workspace, &task, false);
        let mut routine = pilot_routine();
        assert_fresh(&workspace, &routine, "fresh");
        workspace.commit_file(
            "unrelated.rs",
            "fn unrelated() {}\n",
            "unrelated source move",
        );
        assert_fresh(&workspace, &routine, "fresh");
        match change {
            "material" => {
                workspace
                    .runtime
                    .run_tool(
                        "orbit.task.update",
                        json!({
                            "id": task.id, "model": "codex", "tags": ["changed"],
                        }),
                    )
                    .unwrap();
            }
            "source" => workspace.commit_file("README.md", "changed\n", "selector source move"),
            "policy" | "consumer" => {
                // Both a changed existing consumer and another consumer must
                // evaluate evidence under their own resolved contract.
                if change == "consumer" {
                    routine.name = "other-pilot".into();
                }
                routine
                    .trigger
                    .state
                    .as_mut()
                    .unwrap()
                    .freshness
                    .source_sensitivity = Some(SourceSensitivity::Any);
            }
            _ => unreachable!(),
        }
        assert_preparation_due(&workspace, &routine, &task);
    }
}

fn newer_unready_explicit_evidence_is_fresh_without_releasing_active_pilot_holds() {
    let workspace = Workspace::new();
    let task = workspace.task("fresh unready verdict");
    let routine = pilot_routine();
    assess(&workspace, &task, false);
    assert_fresh(&workspace, &routine, "fresh");
    assess(&workspace, &task, true);
    assert_fresh(&workspace, &routine, "fresh_unready");
    let holder = workspace.hold(&[&task.id]);
    let held = evaluate_routine(&workspace.runtime, &routine, false, Utc::now()).unwrap();
    assert_eq!(held.reason, "work_withheld");
    assert_eq!(
        held.state.unwrap().members.unwrap().withheld[&task.id],
        format!("already_preparing: {holder}")
    );
    workspace
        .jobs
        .finalize_job_run(&holder, JobRunState::Success, Utc::now(), None)
        .unwrap();
    let settled = evaluate_routine(&workspace.runtime, &routine, false, Utc::now()).unwrap();
    assert_eq!(settled.reason, "fresh_unready");
    assert!(settled.batch.is_empty());
}

fn admission_reuses_an_explicit_assessment_off_the_scan_page() {
    let workspace = Workspace::new();
    for index in 0..50 {
        workspace
            .runtime
            .add_task(TaskAddParams {
                title: format!("opted out {index}"),
                tags: vec!["no-diff-expected".into()],
                status: Some(TaskStatus::Proposed),
                ..Default::default()
            })
            .unwrap();
    }
    let task = workspace
        .runtime
        .add_task(TaskAddParams {
            title: "newest pending member".into(),
            description: "Inspect README.md.".into(),
            plan: "Inspect README.md.".into(),
            acceptance_criteria: vec!["The README repair is observable.".into()],
            context_files: vec!["file:README.md".into()],
            status: Some(TaskStatus::Proposed),
            ..Default::default()
        })
        .unwrap();
    let routine = pilot_routine();
    let now = Utc::now();
    let observed = evaluate_routine(&workspace.runtime, &routine, false, now).unwrap();
    let members = observed.state.unwrap().members.unwrap();
    assert!(members.pending.contains_key(&task.id));
    assert!(members.scan_after.is_some());
    assess(&workspace, &task, false);
    let skipped = evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(3),
    )
    .unwrap();
    assert!(skipped.batch.is_empty(), "{skipped:?}");
    let members = skipped.state.unwrap().members.unwrap();
    assert!(!members.pending.contains_key(&task.id));
    assert!(members.active.is_none());
    assert!(
        workspace
            .jobs
            .list_job_runs("task_pilot_pipeline")
            .unwrap()
            .is_empty()
    );
}

crate::scenario_table!(
    explicit_assessment_freshness_scenarios,
    explicit_assessment_survives_selector_apply_promotion_and_administrative_edits,
    routine_reuse_obeys_configured_material_source_and_consumer_policy,
    newer_unready_explicit_evidence_is_fresh_without_releasing_active_pilot_holds,
    admission_reuses_an_explicit_assessment_off_the_scan_page
);
