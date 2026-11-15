//! Private history-read fault seam of the `resolves` side effects: the failure
//! must land exactly between a committed transition into done and its
//! post-commit effects, which no public boundary can time deterministically.

use chrono::Utc;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_store::friction_store::{FrictionAddParams, FrictionStore};
use orbit_types::record::{FrictionRecord, FrictionStatus};
use orbit_types::task::{TaskRelation, TaskRelationType, TaskStatus};
use serde_json::json;

use super::{enter_isolated_child, test_runtime};
use crate::application::task::resolves::FAIL_HISTORY_READ;
use crate::application::task::{TaskAddParams, TaskUpdateParams};

/// The repaired read failure: a description-only, a criterion-only and a
/// relation target stay owed through one failed read, then resolve once with
/// the delivery's own commit and PR, leaving an already-resolved friction alone.
#[test]
fn landing_keeps_every_named_target_through_a_failed_history_read() {
    if !enter_isolated_child(
        module_path!(),
        "landing_keeps_every_named_target_through_a_failed_history_read",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let frictions = FrictionStore::open(
        runtime.sqlite_store().expect("store"),
        runtime.workspace_id().expect("workspace id"),
        runtime.data_root().join("frictions"),
    )
    .expect("friction store");
    let add = |status| {
        frictions
            .add(FrictionAddParams {
                model: "codex".to_string(),
                title: None,
                body: "Original report".to_string(),
                tags: vec!["tooling".to_string()],
                status,
                during_task: None,
                created_at: Utc::now(),
            })
            .expect("add friction")
            .record
    };
    let described = add(FrictionStatus::Open);
    let criterion = add(FrictionStatus::Open);
    let related = add(FrictionStatus::Open);
    let settled = add(FrictionStatus::Resolved);
    let targets = [&described, &criterion, &related];
    let show = |record: &FrictionRecord| -> FrictionRecord {
        frictions.show(&record.id).unwrap().unwrap().record
    };

    let task = runtime
        .add_task(TaskAddParams {
            title: "Landing resolves its frictions".to_string(),
            description: format!("Fix {}; {} is already settled.", described.id, settled.id),
            acceptance_criteria: vec![format!("Friction {} is resolved.", criterion.id)],
            plan: "1) implement 2) validate".to_string(),
            relations: vec![TaskRelation {
                relation_type: TaskRelationType::Resolves,
                target: related.id.clone(),
                regression: None,
            }],
            status: Some(TaskStatus::InProgress),
            ..Default::default()
        })
        .expect("add task");
    runtime
        .apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                ..Default::default()
            },
        )
        .expect("to review");
    let commit = "e".repeat(40);
    FAIL_HISTORY_READ.with(|fail| fail.set(true));
    runtime
        .apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Done),
                expected_status: Some(TaskStatus::Review),
                status_note: Some(format!(
                    "completion authorized; delivered by pull request #42 merged as {commit}"
                )),
                ..Default::default()
            },
        )
        .expect("the committed landing survives the failed read");
    assert_eq!(runtime.get_task(&task.id).unwrap().status, TaskStatus::Done);
    for target in targets {
        assert_eq!(
            show(target),
            *target,
            "nothing resolves without its evidence"
        );
    }

    let write = |comment: &str| {
        runtime
            .update_task_with_identity(
                &task.id,
                TaskUpdateParams {
                    comment: Some(comment.to_string()),
                    ..Default::default()
                },
                Some("codex".to_string()),
                None,
            )
            .expect("write to the done task");
    };
    write("A later write retries the landing's targets.");
    for target in targets {
        let resolved = show(target);
        assert_eq!(resolved.status, FrictionStatus::Resolved);
        assert_eq!(resolved.resolved_by_task.as_deref(), Some(task.id.as_str()));
        let appended = resolved.body.strip_prefix(&target.body).expect("body kept");
        assert!(
            appended.contains(&format!("commit {commit}; PR #42")),
            "{appended}"
        );
    }
    assert_eq!(show(&settled), settled);

    // Settled: a reopened target stays open, and its body gains nothing twice.
    let resolved_body = show(&described).body;
    runtime
        .run_tool(
            "orbit.friction.update",
            json!({"id": described.id, "status": "open"}),
        )
        .expect("human reopens target");
    write("A write after settlement resolves nothing.");
    let reopened = show(&described);
    assert_eq!(reopened.status, FrictionStatus::Open);
    assert_eq!(reopened.body, resolved_body);
}
