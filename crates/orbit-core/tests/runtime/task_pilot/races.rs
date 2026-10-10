//! Admission and operator interleavings through the composed runtime.

use orbit_core::TaskComplexity;
use orbit_core::application::task::TaskUpdateParams;

use super::*;

fn assessment(task: &Value) -> Value {
    json!({
        "task_id": task["task_id"],
        "context_files_before": task["context_files_before"],
        "context_files_after": ["file:README.md"],
        "disposition": "selectors", "recommended_crew": "fixture",
        "recommended_complexity": "low", "confidence": "high",
        "assessment_rationale": "README.md contains the affected material.",
        "validation_approach": "Inspect the persisted assessment.",
        "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
        "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
        "duplicate_of": null, "already_landed": null,
    })
}

pub(super) fn apply_input(workspace: &Workspace, prepared: &Value) -> Value {
    json!({
        "workspace_path": workspace.repo,
        "prepared": prepared,
        "results": prepared["partitions"].as_array().unwrap().iter().map(|partition| {
            let tasks = prepared["tasks"].as_array().unwrap().iter()
                .filter(|task| partition["task_ids"].as_array().unwrap().contains(&task["task_id"]))
                .map(assessment).collect::<Vec<_>>();
            json!({"partition_index": partition["partition_index"], "task_ids": partition["task_ids"], "tasks": tasks})
        }).collect::<Vec<_>>(),
    })
}

fn assert_superseded(workspace: &Workspace, output: &Value, count: usize) {
    assert_eq!(output["status"], "succeeded", "{output}");
    assert_eq!(output["outcome"], "superseded", "{output}");
    assert_eq!(output["superseded_count"], count, "{output}");
    assert_eq!(output["unresolved_count"], 0, "{output}");
    assert!(output["error"].is_null(), "{output}");
    assert_eq!(
        workspace.action("pipeline_success_guard", json!({"result": output}))["succeeded"],
        true
    );
}

#[test]
fn admitted_and_retired_targets_settle_as_superseded_without_pilot_writes() {
    if !super::super::dispatch_admission::isolated(
        "task_pilot::races::admitted_and_retired_targets_settle_as_superseded_without_pilot_writes",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let tasks = [
        "admitted",
        "implementer plan",
        "rejected",
        "archived",
        "someday",
    ]
    .map(|title| workspace.task(title));
    for task in &tasks[..2] {
        workspace
            .runtime
            .update_task_as_human(
                &task.id,
                TaskUpdateParams {
                    status: Some(TaskStatus::Backlog),
                    ..Default::default()
                },
                "fixture".into(),
            )
            .unwrap();
    }
    let ids = tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<Vec<_>>();
    let prepared = workspace.prepare(&ids);
    for task in &tasks[..2] {
        workspace
            .runtime
            .admit_task_for_workflow(&task.id, "worktree_setup")
            .unwrap();
    }
    workspace
        .runtime
        .update_task_with_identity(
            &tasks[1].id,
            TaskUpdateParams {
                plan: Some("The admitted implementer's concrete plan.".into()),
                ..Default::default()
            },
            Some("codex".into()),
            None,
        )
        .unwrap();
    for (task, status) in tasks[2..].iter().zip([
        TaskStatus::Rejected,
        TaskStatus::Archived,
        TaskStatus::Someday,
    ]) {
        workspace
            .runtime
            .update_task_as_human(
                &task.id,
                TaskUpdateParams {
                    status: Some(status),
                    ..Default::default()
                },
                "fixture".into(),
            )
            .unwrap();
    }
    let before = ids
        .iter()
        .map(|id| workspace.runtime.get_task(id).unwrap())
        .collect::<Vec<_>>();
    let mut input = apply_input(&workspace, &prepared);
    // The operator's rejection can also make the agent's reported snapshot
    // stale. No metadata from this assessment may be written [ORB-14297].
    input["results"][0]["tasks"][2]["context_files_before"] = json!(["file:other.rs"]);
    let output = workspace.action("apply_task_pilot_results", input);
    assert_superseded(&workspace, &output, tasks.len());
    assert_eq!(output["applied_count"], 0);
    for task in &before {
        assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), *task);
        assert!(
            workspace
                .runtime
                .get_task_history(&task.id)
                .unwrap()
                .iter()
                .all(|entry| entry.event != "task_pilot_applied")
        );
    }
}

#[test]
fn human_material_edits_supersede_preparation_without_writes() {
    if !super::super::dispatch_admission::isolated(
        "task_pilot::races::human_material_edits_supersede_preparation_without_writes",
    ) {
        return;
    }
    let workspace = Workspace::new();
    for admitted in [false, true] {
        for field in ["description", "plan", "context"] {
            let task = workspace.task(&format!("edit {field} admitted {admitted}"));
            workspace
                .runtime
                .update_task_as_human(
                    &task.id,
                    TaskUpdateParams {
                        status: Some(TaskStatus::Backlog),
                        ..Default::default()
                    },
                    "fixture".into(),
                )
                .unwrap();
            let prepared = workspace.prepare(&[&task.id]);
            if admitted {
                workspace
                    .runtime
                    .admit_task_for_workflow(&task.id, "worktree_setup")
                    .unwrap();
            }
            let params = match field {
                "description" => TaskUpdateParams {
                    description: Some("Operator changed the mandate.".into()),
                    ..Default::default()
                },
                "plan" => TaskUpdateParams {
                    plan: Some("Operator changed the plan.".into()),
                    ..Default::default()
                },
                _ => TaskUpdateParams {
                    context_files: Some(vec!["file:README.md".into()]),
                    ..Default::default()
                },
            };
            let edited = workspace
                .runtime
                .update_task_as_human(&task.id, params, "fixture".into())
                .unwrap();
            let mut input = apply_input(&workspace, &prepared);
            if field == "context" {
                input["results"][0]["tasks"][0]["context_files_before"] =
                    json!(edited.context_files);
            }
            let output = workspace.action("apply_task_pilot_results", input);
            assert_superseded(&workspace, &output, 1);
            assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), edited);
        }
    }
}

#[test]
fn agent_before_echo_is_advisory_and_replays_use_the_prepared_snapshot() {
    if !super::super::dispatch_admission::isolated(
        "task_pilot::races::agent_before_echo_is_advisory_and_replays_use_the_prepared_snapshot",
    ) {
        return;
    }
    let workspace = Workspace::new();
    workspace.commit_file("src/lib.rs", "fn fixture() {}\n", "add source");
    for (case, echo) in [
        ("reordered", Some(json!(["dir:src", "file:README.md"]))),
        ("normalized", Some(json!(["./README.md", "file:src"]))),
        ("different", Some(json!(["file:other.rs"]))),
        ("omitted", None),
    ] {
        let task = workspace.task(case);
        workspace
            .runtime
            .update_task_as_human(
                &task.id,
                TaskUpdateParams {
                    context_files: Some(vec!["file:README.md".into(), "dir:src".into()]),
                    ..Default::default()
                },
                "fixture".into(),
            )
            .unwrap();
        let prepared = workspace.prepare(&[&task.id]);
        let mut input = apply_input(&workspace, &prepared);
        let assessment = &mut input["results"][0]["tasks"][0];
        if let Some(echo) = echo {
            assessment["context_files_before"] = echo;
        } else {
            assessment
                .as_object_mut()
                .unwrap()
                .remove("context_files_before");
        }
        let output = workspace.action("apply_task_pilot_results", input);
        assert_eq!(output["status"], "succeeded", "{case}: {output}");
        assert_eq!(output["applied_count"], 1, "{case}: {output}");
        assert_eq!(
            output["tasks"][0]["context_files_before"],
            prepared["tasks"][0]["context_files_before"]
        );
        let applied = workspace.runtime.get_task(&task.id).unwrap();
        assert_eq!(applied.context_files, ["file:README.md"]);
        let history = workspace.runtime.get_task_history(&task.id).unwrap();
        let mut replay = apply_input(&workspace, &prepared);
        replay["results"][0]["tasks"][0]["context_files_before"] = json!(["a different echo"]);
        let replayed = workspace.action("apply_task_pilot_results", replay);
        assert_eq!(replayed["status"], "succeeded", "{case}: {replayed}");
        assert_eq!(replayed["tasks"][0]["outcome"], "already_applied");
        assert_eq!(
            replayed["tasks"][0]["operation_id"],
            output["tasks"][0]["operation_id"]
        );
        assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), applied);
        assert_eq!(
            workspace.runtime.get_task_history(&task.id).unwrap(),
            history
        );
    }
}

#[test]
fn promoting_ci_sweep_replay_settles_already_applied_without_writes() {
    if !super::super::dispatch_admission::isolated(
        "task_pilot::races::promoting_ci_sweep_replay_settles_already_applied_without_writes",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let task = workspace.task("promoting CI-sweep replay");
    workspace
        .runtime
        .update_task_as_human(
            &task.id,
            TaskUpdateParams {
                tags: Some(vec![
                    "ci-failure-sweep".into(),
                    "ci-failure:fixture-key".into(),
                ]),
                ..Default::default()
            },
            "fixture".into(),
        )
        .unwrap();
    let prepared = workspace.prepare(&[&task.id]);
    let mut input = apply_input(&workspace, &prepared);
    input["ci_sweep_filing"] = json!({
        "task_id": task.id,
        "failure_key": "fixture-key",
        "tested_commit": workspace.object("HEAD"),
        "workflow": "ci", "job": "test", "step": "cargo test",
        "run_urls": ["https://github.com/example/repo/actions/runs/1"],
    });
    input["promotion_authorized"] = json!(true);
    let output = workspace.action("apply_task_pilot_results", input.clone());
    assert_eq!(output["status"], "succeeded", "{output}");
    assert_eq!(output["tasks"][0]["outcome"], "applied", "{output}");
    assert_eq!(output["ci_sweep_admission"][0]["decision"], "promote");
    let applied = workspace.runtime.get_task(&task.id).unwrap();
    assert_eq!(applied.status, TaskStatus::Backlog);
    assert_eq!(applied.context_files, ["file:README.md"]);
    let history = workspace.runtime.get_task_history(&task.id).unwrap();

    // The atomic promotion landed, but the step checkpoint was lost. The
    // engine retries the identical input against the now-backlog task.
    let replayed = workspace.action("apply_task_pilot_results", input.clone());
    assert_eq!(replayed["status"], "succeeded", "{replayed}");
    assert_eq!(replayed["tasks"][0]["outcome"], "already_applied");
    assert_eq!(replayed["applied_count"], 1);
    assert_eq!(replayed["unresolved_count"], 0);
    assert_eq!(replayed["repair_partitions"], json!([]));
    assert_eq!(replayed["ci_sweep_admission"], output["ci_sweep_admission"]);
    assert_eq!(
        replayed["tasks"][0]["operation_id"],
        output["tasks"][0]["operation_id"]
    );
    assert_eq!(
        workspace.action("pipeline_success_guard", json!({"result": replayed}))["succeeded"],
        true
    );
    assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), applied);
    assert_eq!(
        workspace.runtime.get_task_history(&task.id).unwrap(),
        history
    );

    // A different operation cannot borrow the committed promotion's receipt:
    // to it the task was promoted elsewhere, so it admits and writes nothing.
    input["results"][0]["tasks"][0]["assessment_rationale"] =
        json!("A new assessment of the failure.");
    let changed = workspace.action("apply_task_pilot_results", input);
    assert_eq!(changed["status"], "succeeded", "{changed}");
    assert_eq!(changed["applied_count"], 0, "{changed}");
    assert_eq!(changed["ci_sweep_admission"], json!([]), "{changed}");
    assert_eq!(changed["task_outcomes"][0]["reason"], "promoted_elsewhere");
    assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), applied);
    assert_eq!(
        workspace.runtime.get_task_history(&task.id).unwrap(),
        history
    );
}

#[test]
fn superseded_sibling_survives_targeted_repair_and_a_valid_sibling_applies() {
    if !super::super::dispatch_admission::isolated(
        "task_pilot::races::superseded_sibling_survives_targeted_repair_and_a_valid_sibling_applies",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let tasks = ["retired", "repair", "valid"].map(|title| workspace.task(title));
    let prepared = workspace.prepare(
        &tasks
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>(),
    );
    workspace
        .runtime
        .update_task_as_human(
            &tasks[0].id,
            TaskUpdateParams {
                status: Some(TaskStatus::Rejected),
                ..Default::default()
            },
            "fixture".into(),
        )
        .unwrap();
    let mut input = apply_input(&workspace, &prepared);
    input["results"][0]["tasks"][1]["recommended_complexity"] = json!("invalid");
    let first = workspace.action("apply_task_pilot_results", input);
    assert_eq!(first["status"], "failed");
    assert_eq!(first["repair_count"], 1);
    assert_eq!(first["applied_count"], 1);
    let mut repair = apply_input(&workspace, &first["repair_prepared"]);
    repair["prior_applied_count"] = first["applied_count"].clone();
    repair["carried_task_outcomes"] = first["non_repairable_outcomes"].clone();
    let output = workspace.action("apply_task_pilot_results", repair);
    assert_superseded(&workspace, &output, 1);
    assert_eq!(output["applied_count"], 2);
}

#[test]
fn local_auto_ship_and_readiness_wait_for_an_active_pilot_checkpoint() {
    if !super::super::dispatch_admission::isolated(
        "task_pilot::races::local_auto_ship_and_readiness_wait_for_an_active_pilot_checkpoint",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let task = workspace.task("ready to deliver");
    workspace
        .runtime
        .update_task_as_human(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Backlog),
                context_files: Some(vec!["file:README.md".into()]),
                complexity: Some(TaskComplexity::Low),
                ..Default::default()
            },
            "fixture".into(),
        )
        .unwrap();
    let holder = workspace.hold(&[&task.id]);
    for input in [json!({}), json!({"task_ids": [task.id]})] {
        let output = workspace.action("list_backlog_tasks", input);
        assert_eq!(output["task_ids"], json!([]), "{output}");
        assert_eq!(
            output["excluded"][0]["reason"], "active_pilot_preparation",
            "{output}"
        );
    }
    let wave = workspace.action(
        "classify_workspace_auto_tasks",
        json!({"max_active_leaf_runs": 1}),
    );
    assert_eq!(wave["loose_task_ids"], json!([]), "{wave}");
    let ready = workspace
        .runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    assert_eq!(
        ready["tasks"][0]["reason"], "active_pilot_preparation",
        "{ready}"
    );
    workspace
        .jobs
        .finalize_job_run(&holder, JobRunState::Success, Utc::now(), None)
        .unwrap();
    assert_eq!(
        workspace.action("list_backlog_tasks", json!({}))["task_ids"],
        json!([task.id])
    );
}
