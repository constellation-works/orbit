use orbit_types::task::{TaskComplexity, TaskPriority, TaskStatus, TaskType};
use serde_json::json;

use crate::adapter::engine_host::v2_host::test_support::{
    runtime_with_workspace_layout, seed_list_backlog_task, write_workspace_file,
};
use crate::application::task::{TaskAddParams, TaskUpdateParams};

use super::support::{
    classify_with, readiness, readiness_task, seed_backlog_leaves, seed_live_leaf_run,
    seed_running_drain_input, seed_unassessed_task,
};

#[test]
fn readiness_explains_dependencies_locks_children_claims_and_capacity() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "crates/locked/src/lib.rs");
    let dependency = seed_list_backlog_task(
        &runtime,
        "Unfinished dependency",
        TaskStatus::Proposed,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec![],
    );
    let blocked = runtime
        .add_task(TaskAddParams {
            title: "Blocked leaf".to_string(),
            description: "fixture".to_string(),
            acceptance_criteria: vec!["fixture".to_string()],
            plan: "fixture".to_string(),
            dependencies: vec![dependency.id.clone()],
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed blocked leaf");
    let missing_dependency = seed_list_backlog_task(
        &runtime,
        "Deleted dependency",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec![],
    );
    let missing = runtime
        .add_task(TaskAddParams {
            title: "Missing dependency leaf".to_string(),
            description: "fixture".to_string(),
            acceptance_criteria: vec!["fixture".to_string()],
            plan: "fixture".to_string(),
            dependencies: vec![missing_dependency.id.clone()],
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed missing dependency leaf");
    runtime
        .delete_task(&missing_dependency.id)
        .expect("delete dependency for missing fixture");
    let _holder = seed_list_backlog_task(
        &runtime,
        "Lock holder",
        TaskStatus::InProgress,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec!["crates/locked/src/lib.rs"],
    );
    let locked = seed_list_backlog_task(
        &runtime,
        "Locked leaf",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec!["crates/locked/src/lib.rs"],
    );
    // [ORB-12491] A child of an `epic`-tagged root is an ordinary leaf: it is
    // never "managed", and only the scarce slot keeps it waiting.
    let tagged_root = runtime
        .add_task(TaskAddParams {
            title: "Tagged root".to_string(),
            description: "fixture".to_string(),
            acceptance_criteria: vec!["fixture".to_string()],
            plan: "fixture".to_string(),
            tags: vec!["epic".to_string()],
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed tagged root");
    let child = seed_list_backlog_task(
        &runtime,
        "Child of a tagged root",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        Some(tagged_root.id),
        vec![],
    );
    let claimed = seed_list_backlog_task(
        &runtime,
        "Claimed leaf",
        TaskStatus::Backlog,
        TaskPriority::High,
        TaskType::Chore,
        None,
        vec![],
    );
    let claim_run = seed_live_leaf_run(&runtime, &[&claimed.id]);
    let saturated = seed_list_backlog_task(
        &runtime,
        "Capacity leaf",
        TaskStatus::Backlog,
        TaskPriority::Low,
        TaskType::Chore,
        None,
        vec![],
    );

    let ids = vec![
        blocked.id.clone(),
        missing.id.clone(),
        locked.id.clone(),
        child.id.clone(),
        claimed.id.clone(),
        saturated.id.clone(),
    ];
    let output = readiness(&runtime, &ids, Some(1));

    assert_eq!(
        readiness_task(&output, &blocked.id)["reason"],
        "unmet_dependency"
    );
    assert_eq!(
        readiness_task(&output, &missing.id)["dependencies"][0]["status"],
        "missing"
    );
    assert_eq!(
        readiness_task(&output, &locked.id)["reason"],
        "context_lock_conflict"
    );
    assert_eq!(
        readiness_task(&output, &child.id)["reason"],
        "capacity_saturated"
    );
    assert_eq!(
        readiness_task(&output, &claimed.id)["reason"],
        "claimed_by_live_child"
    );
    assert_eq!(
        readiness_task(&output, &claimed.id)["run_ids"],
        json!([claim_run])
    );
    assert_eq!(
        readiness_task(&output, &saturated.id)["reason"],
        "capacity_saturated"
    );
}

/// Promotion and readiness share dispatch's archived-dependency rule: a
/// dependency archived after `done` releases its dependent, one archived
/// without reaching `done` keeps it an unmet dependency.
#[test]
fn readiness_releases_only_dependents_of_work_archived_after_done() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let seed_dependency = |title: &str| {
        seed_list_backlog_task(
            &runtime,
            title,
            TaskStatus::Backlog,
            TaskPriority::Medium,
            TaskType::Chore,
            None,
            vec![],
        )
        .id
    };
    let seed_dependent = |title: &str, dependency: &str| {
        runtime
            .add_task(TaskAddParams {
                title: title.to_string(),
                description: "fixture".to_string(),
                acceptance_criteria: vec!["fixture".to_string()],
                plan: "fixture".to_string(),
                dependencies: vec![dependency.to_string()],
                complexity: TaskComplexity::Medium,
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            })
            .expect("seed dependent")
            .id
    };
    let finished = seed_dependency("Finished then archived");
    let abandoned = seed_dependency("Abandoned then archived");
    runtime
        .update_task(
            &finished,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
        )
        .expect("finish dependency");
    runtime.archive_task(&finished).expect("archive finished");
    runtime.archive_task(&abandoned).expect("archive abandoned");
    let released = seed_dependent("Depends on finished work", &finished);
    let stranded = seed_dependent("Depends on abandoned work", &abandoned);

    let output = readiness(&runtime, &[released.clone(), stranded.clone()], Some(2));

    assert_eq!(readiness_task(&output, &released)["reason"], "ready");
    assert_eq!(
        readiness_task(&output, &stranded)["reason"],
        "unmet_dependency"
    );
    assert_eq!(
        readiness_task(&output, &stranded)["dependencies"],
        json!([{ "task_id": abandoned, "status": "archived" }])
    );
}

#[test]
fn readiness_matches_dispatch_and_does_not_mutate_the_snapshot() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let first = seed_list_backlog_task(
        &runtime,
        "First ready leaf",
        TaskStatus::Backlog,
        TaskPriority::High,
        TaskType::Chore,
        None,
        vec![],
    );
    let second = seed_list_backlog_task(
        &runtime,
        "Second ready leaf",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec![],
    );
    let ids = vec![second.id.clone(), first.id.clone()];
    let before_runs = runtime
        .stores()
        .jobs()
        .list_pending_or_running_job_runs("task_auto_pipeline")
        .expect("list runs");

    let output = readiness(&runtime, &ids, Some(1));
    let dispatched = classify_with(&runtime, json!({ "max_active_leaf_runs": 1 }));

    assert_eq!(readiness_task(&output, &first.id)["reason"], "ready");
    assert_eq!(readiness_task(&output, &first.id)["eligible"], true);
    assert_eq!(
        readiness_task(&output, &second.id)["reason"],
        "capacity_saturated"
    );
    assert_eq!(dispatched["loose_task_ids"], json!([first.id]));
    assert_eq!(
        runtime
            .stores()
            .jobs()
            .list_pending_or_running_job_runs("task_auto_pipeline")
            .expect("list runs after"),
        before_runs
    );
    assert_eq!(
        runtime.get_task(&second.id).expect("read task").status,
        TaskStatus::Backlog
    );
    assert!(
        output["snapshot"]["limitations"]
            .as_str()
            .expect("limitations")
            .contains("does not guarantee")
    );
}

#[test]
fn readiness_uses_the_classifier_candidate_pool() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "crates/shared/src/lib.rs");
    write_workspace_file(&repo_root, "crates/outside/src/lib.rs");

    let mut task_ids = (0..50)
        .map(|index| {
            seed_list_backlog_task(
                &runtime,
                &format!("Conflicting candidate {index}"),
                TaskStatus::Backlog,
                TaskPriority::Medium,
                TaskType::Chore,
                None,
                vec!["crates/shared/src/lib.rs"],
            )
            .id
        })
        .collect::<Vec<_>>();
    let outside_pool = seed_list_backlog_task(
        &runtime,
        "Conflict-free task outside the candidate pool",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec!["crates/outside/src/lib.rs"],
    );
    task_ids.push(outside_pool.id.clone());

    let classified = classify_with(&runtime, json!({ "max_active_leaf_runs": 4 }));
    let readiness = runtime
        .workspace_auto_readiness(&task_ids, Some(4), 51, &[])
        .expect("explain readiness");
    let readiness_admitted = readiness["tasks"]
        .as_array()
        .expect("readiness tasks")
        .iter()
        .filter(|task| task["eligible"] == true)
        .map(|task| task["task_id"].clone())
        .collect::<Vec<_>>();

    let classified_admitted = classified["loose_task_ids"]
        .as_array()
        .expect("classified task ids");
    assert_eq!(&readiness_admitted, classified_admitted);
    assert_eq!(readiness["capacity"]["candidate_pool_size"], 50);
    assert_eq!(readiness["capacity"]["candidate_pool_truncated"], true);
    assert_eq!(
        readiness_task(&readiness, &outside_pool.id)["reason"],
        "outside_candidate_pool"
    );
    assert_eq!(
        readiness_task(&readiness, &outside_pool.id)["eligible"],
        false
    );
}

#[test]
fn readiness_uses_the_active_drain_candidate_pool_for_a_conflict_free_tail() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "crates/shared/src/lib.rs");
    write_workspace_file(&repo_root, "crates/outside/src/lib.rs");

    let mut task_ids = (0..50)
        .map(|index| {
            seed_list_backlog_task(
                &runtime,
                &format!("Conflicting candidate {index}"),
                TaskStatus::Backlog,
                TaskPriority::Medium,
                TaskType::Chore,
                None,
                vec!["crates/shared/src/lib.rs"],
            )
            .id
        })
        .collect::<Vec<_>>();
    let outside_pool = seed_list_backlog_task(
        &runtime,
        "Conflict-free task after the candidate pool",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec!["crates/outside/src/lib.rs"],
    );
    task_ids.push(outside_pool.id.clone());

    let drain_run_id = seed_running_drain_input(
        &runtime,
        json!({ "max_active_leaf_runs": 4, "max_tasks": "51" }),
    );
    let classified = classify_with(
        &runtime,
        json!({
            "run_id": drain_run_id,
            "max_active_leaf_runs": 4,
            "max_tasks": "51",
        }),
    );
    let readiness = runtime
        .workspace_auto_readiness(&task_ids, None, 51, &[])
        .expect("explain readiness");
    let readiness_admitted = readiness["tasks"]
        .as_array()
        .expect("readiness tasks")
        .iter()
        .filter(|task| task["eligible"] == true)
        .map(|task| task["task_id"].clone())
        .collect::<Vec<_>>();

    assert_eq!(
        &readiness_admitted,
        classified["loose_task_ids"]
            .as_array()
            .expect("classified task ids")
    );
    assert_eq!(readiness["capacity"]["candidate_pool_size"], 51);
    assert_eq!(readiness["capacity"]["candidate_pool_truncated"], false);
    assert_eq!(
        readiness_task(&readiness, &outside_pool.id)["reason"],
        "ready"
    );
    assert_eq!(
        readiness_task(&readiness, &outside_pool.id)["eligible"],
        true
    );
}

#[test]
fn readiness_candidate_pool_parsing_matches_classifier_for_numeric_and_string_one() {
    for max_tasks in [json!(1), json!("1")] {
        let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
        let tasks = seed_backlog_leaves(&runtime, 2);
        let drain_run_id = seed_running_drain_input(
            &runtime,
            json!({ "max_active_leaf_runs": 2, "max_tasks": max_tasks }),
        );
        let input = json!({
            "run_id": drain_run_id,
            "max_active_leaf_runs": 2,
            "max_tasks": max_tasks,
        });

        let classified = classify_with(&runtime, input);
        let readiness = runtime
            .workspace_auto_readiness(&tasks, None, tasks.len(), &[])
            .expect("explain readiness");

        assert_eq!(classified["candidate_pool_size"], 1);
        assert_eq!(readiness["capacity"]["candidate_pool_size"], 1);
        assert_eq!(classified["loose_task_ids"], json!([tasks[0]]));
        assert_eq!(readiness_task(&readiness, &tasks[0])["reason"], "ready");
        assert_eq!(
            readiness_task(&readiness, &tasks[1])["reason"],
            "outside_candidate_pool"
        );
    }
}

#[test]
fn readiness_agrees_with_admission_on_the_no_diff_expected_complexity_exemption() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let exempt = seed_unassessed_task(&runtime, "Operational check", &["no-diff-expected"]);
    let implementation = seed_unassessed_task(&runtime, "Repair the finding", &["code-review"]);
    let blocked_dependency = seed_list_backlog_task(
        &runtime,
        "Unfinished dependency",
        TaskStatus::InProgress,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec![],
    );
    let dependent_exempt = runtime
        .add_task(TaskAddParams {
            title: "Operational check behind a dependency".to_string(),
            description: "Fixture task".to_string(),
            acceptance_criteria: vec!["Fixture outcome is observable.".to_string()],
            dependencies: vec![blocked_dependency.id.clone()],
            tags: vec!["no-diff-expected".to_string()],
            plan: "Fixture plan.".to_string(),
            priority: TaskPriority::Medium,
            complexity: TaskComplexity::Unassessed,
            task_type: Some(TaskType::Chore),
            status: Some(TaskStatus::Backlog),
            ..TaskAddParams::default()
        })
        .expect("seed dependent exempt task");

    let output = readiness(
        &runtime,
        &[
            exempt.id.clone(),
            implementation.id.clone(),
            dependent_exempt.id.clone(),
        ],
        None,
    );

    assert_eq!(readiness_task(&output, &exempt.id)["eligible"], json!(true));
    assert_eq!(readiness_task(&output, &exempt.id)["reason"], "ready");
    assert_eq!(
        readiness_task(&output, &implementation.id)["reason"],
        "task_pilot_preparation_required"
    );
    assert_eq!(
        readiness_task(&output, &implementation.id)["eligible"],
        json!(false)
    );
    // Other exclusions stay visible and enforced for an exempt task.
    assert_eq!(
        readiness_task(&output, &dependent_exempt.id)["reason"],
        "unmet_dependency"
    );
    assert_eq!(
        readiness_task(&output, &dependent_exempt.id)["eligible"],
        json!(false)
    );
    // Admission itself never rewrites the stored non-answer.
    assert_eq!(
        runtime
            .get_task(&exempt.id)
            .expect("exempt task")
            .complexity,
        Some(TaskComplexity::Unassessed)
    );
}
