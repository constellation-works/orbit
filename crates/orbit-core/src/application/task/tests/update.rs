//! Explicit status classification through the owning `update_task` layer.

use orbit_engine::TaskActivityUpdate;
use orbit_types::task::{Task, TaskArtifact, TaskStatus};

use super::test_runtime;
use crate::OrbitRuntime;
use crate::application::task::{TaskAddParams, TaskUpdateParams};

fn add_proposed_task(runtime: &OrbitRuntime, title: &str) -> Task {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: "Exercise guarded update transitions.".to_string(),
            acceptance_criteria: vec!["status lands where the update says.".to_string()],
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("add proposed task")
}

fn update_status(
    runtime: &OrbitRuntime,
    id: &str,
    status: TaskStatus,
) -> Result<Task, orbit_common::OrbitError> {
    runtime.update_task(
        id,
        TaskUpdateParams {
            status: Some(status),
            ..Default::default()
        },
    )
}

const ALL_STATUSES: [TaskStatus; 9] = [
    TaskStatus::Proposed,
    TaskStatus::Backlog,
    TaskStatus::InProgress,
    TaskStatus::Review,
    TaskStatus::Done,
    TaskStatus::Blocked,
    TaskStatus::Archived,
    TaskStatus::Rejected,
    TaskStatus::Someday,
];

#[test]
fn explicit_update_allows_the_complete_status_transition_matrix() {
    let (_root, runtime) = test_runtime();

    for source in ALL_STATUSES {
        for target in ALL_STATUSES {
            let task = add_proposed_task(&runtime, &format!("Matrix {source} to {target}"));
            update_status(&runtime, &task.id, source).expect("establish matrix source");
            let history_before = runtime
                .get_task_history(&task.id)
                .expect("history before transition");

            let updated = update_status(&runtime, &task.id, target)
                .unwrap_or_else(|error| panic!("{source} -> {target} failed: {error}"));

            assert_eq!(updated.id, task.id);
            assert_eq!(updated.status, target);
            let history_after = runtime
                .get_task_history(&task.id)
                .expect("history after transition");
            if source == target {
                assert_eq!(history_after, history_before, "{source} same-status edit");
            } else {
                let event = history_after.last().expect("status transition event");
                assert_eq!(event.from_status, Some(source));
                assert_eq!(event.to_status, Some(target));
                assert!(!event.by.trim().is_empty());
                assert!(event.at >= task.created_at);
            }
        }
    }
}

#[test]
fn update_status_covers_approve_transitions() {
    let (_root, runtime) = test_runtime();
    let task = add_proposed_task(&runtime, "Approve via update");

    // proposed -> backlog (the former `task approve`).
    let approved = update_status(&runtime, &task.id, TaskStatus::Backlog)
        .expect("proposed task approves into backlog via update");
    assert_eq!(approved.status, TaskStatus::Backlog);

    // review -> done (the former review approval).
    let done = drive_to_done(&runtime, &task.id);
    assert_eq!(done.status, TaskStatus::Done);
}

#[test]
fn reopening_done_preserves_identity_history_artifacts_and_execution_evidence() {
    let (_root, runtime) = test_runtime();
    let task = add_proposed_task(&runtime, "Preserve reopen evidence");
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                upsert_artifacts: vec![TaskArtifact::from_text("evidence.txt", "durable")],
                ..Default::default()
            },
        )
        .expect("attach evidence before completion");
    let done = drive_to_done(&runtime, &task.id);
    let history_before = runtime.get_task_history(&task.id).expect("done history");
    let artifacts_before = runtime
        .get_task_artifacts(&task.id)
        .expect("done artifacts");

    let reopened = runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Proposed),
                title: Some("Reclassified without replacing evidence".to_string()),
                ..Default::default()
            },
        )
        .expect("reopen done task directly");

    assert_eq!(reopened.id, done.id);
    assert_eq!(reopened.execution_summary, done.execution_summary);
    assert_eq!(reopened.implemented_by, done.implemented_by);
    assert_eq!(
        runtime
            .get_task_artifacts(&task.id)
            .expect("reopened artifacts"),
        artifacts_before
    );
    let history_after = runtime
        .get_task_history(&task.id)
        .expect("reopened history");
    assert_eq!(&history_after[..history_before.len()], history_before);
    let reopen = history_after.last().expect("reopen event");
    assert_eq!(reopen.from_status, Some(TaskStatus::Done));
    assert_eq!(reopen.to_status, Some(TaskStatus::Proposed));
}

#[test]
fn manual_status_edits_do_not_require_execution_fields_or_fabricate_attribution() {
    let (_root, runtime) = test_runtime();
    let task = add_proposed_task(&runtime, "Classification is not execution");

    let in_progress = update_status(&runtime, &task.id, TaskStatus::InProgress)
        .expect("manual in-progress status needs no plan");
    assert!(in_progress.plan.is_empty());
    let review = update_status(&runtime, &task.id, TaskStatus::Review)
        .expect("manual review status needs no summary");
    assert!(review.execution_summary.is_empty());
    assert_eq!(review.implemented_by, None);
}

#[test]
fn invalid_accompanying_edit_does_not_partially_apply_status() {
    let (_root, runtime) = test_runtime();
    let task = add_proposed_task(&runtime, "Atomic invalid edit");

    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                title: Some("   ".to_string()),
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
        )
        .expect_err("invalid title rejects the entire update");

    let unchanged = runtime.get_task(&task.id).expect("unchanged task");
    assert_eq!(unchanged.title, task.title);
    assert_eq!(unchanged.status, TaskStatus::Proposed);
}

#[test]
fn stale_activity_status_write_cannot_overwrite_operator_reclassification() {
    let (_root, runtime) = test_runtime();
    let task = add_proposed_task(&runtime, "Stale activity");
    update_status(&runtime, &task.id, TaskStatus::Review).expect("prepare review snapshot");
    update_status(&runtime, &task.id, TaskStatus::Backlog).expect("operator reclassifies task");

    let error = runtime
        .update_task_from_activity(
            &task.id,
            TaskActivityUpdate {
                status: TaskStatus::Done,
                expected_status: TaskStatus::Review,
                execution_summary: Some("stale worker evidence".to_string()),
                comment: None,
                note: Some("late completion".to_string()),
                agent: Some("codex".to_string()),
                model: None,
            },
        )
        .expect_err("stale activity must lose to the operator");
    assert!(error.to_string().contains("expected 'review'"), "{error}");

    let current = runtime.get_task(&task.id).expect("operator state survives");
    assert_eq!(current.status, TaskStatus::Backlog);
    assert_ne!(current.execution_summary, "stale worker evidence");
}

#[test]
fn orchestrator_is_explicit_mutable_before_start_and_never_routes_execution() {
    let (_root, runtime) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Orchestration ownership".to_string(),
            description: "Keep orchestration attribution separate from execution.".to_string(),
            crew: Some("implementer".to_string()),
            orchestrator: Some("  orchestration  ".to_string()),
            ..Default::default()
        })
        .expect("add task with orchestration attribution");
    assert_eq!(task.orchestrator.as_deref(), Some("orchestration"));
    assert_eq!(
        runtime
            .resolve_crew_for_task(None, task.crew.as_deref())
            .expect("resolve execution crew")
            .name,
        "implementer"
    );

    let changed = runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                orchestrator: Some(Some("  implementer  ".to_string())),
                ..Default::default()
            },
        )
        .expect("change orchestration attribution while proposed");
    assert_eq!(changed.orchestrator.as_deref(), Some("implementer"));
    let cleared = runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                orchestrator: Some(None),
                ..Default::default()
            },
        )
        .expect("clear orchestration attribution while proposed");
    assert_eq!(cleared.orchestrator, None);

    let invalid = runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                orchestrator: Some(Some("missing".to_string())),
                ..Default::default()
            },
        )
        .expect_err("reject unconfigured orchestrator");
    assert!(invalid.to_string().contains("missing"), "{invalid}");

    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                plan: Some("Implement it.".to_string()),
                status: Some(TaskStatus::InProgress),
                ..Default::default()
            },
        )
        .expect("start task");
    let immutable = runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                orchestrator: Some(Some("orchestration".to_string())),
                ..Default::default()
            },
        )
        .expect_err("orchestrator becomes immutable after execution starts");
    assert!(
        immutable.to_string().contains("proposed or backlog"),
        "{immutable}"
    );
}

#[test]
fn orchestrator_is_rejected_on_non_draft_initial_statuses_including_someday() {
    let (_root, runtime) = test_runtime();

    for status in [TaskStatus::Someday, TaskStatus::InProgress] {
        let error = runtime
            .add_task(TaskAddParams {
                title: format!("Invalid {status} orchestration attribution"),
                description: "Orchestration ownership must be assigned before this state."
                    .to_string(),
                status: Some(status),
                orchestrator: Some("orchestration".to_string()),
                ..Default::default()
            })
            .expect_err("non-draft initial status rejects orchestrator");
        assert!(
            error.to_string().contains("proposed or backlog"),
            "{status}: {error}"
        );
    }
}

/// Walks a task through backlog -> in-progress -> review -> done using only
/// `update_task`, satisfying the plan and execution-summary guards.
fn drive_to_done(runtime: &OrbitRuntime, id: &str) -> Task {
    let current = runtime.get_task(id).expect("get task");
    if current.status == TaskStatus::Proposed {
        update_status(runtime, id, TaskStatus::Backlog).expect("proposed -> backlog");
    }
    runtime
        .update_task(
            id,
            TaskUpdateParams {
                plan: Some("1) do the thing 2) verify".to_string()),
                status: Some(TaskStatus::InProgress),
                ..Default::default()
            },
        )
        .expect("backlog -> in-progress with plan");
    runtime
        .update_task(
            id,
            TaskUpdateParams {
                execution_summary: Some("did the thing; verified".to_string()),
                status: Some(TaskStatus::Review),
                ..Default::default()
            },
        )
        .expect("in-progress -> review with execution summary");
    update_status(runtime, id, TaskStatus::Done).expect("review -> done")
}

/// The explicit edit waits for the status write, re-reads under the same task
/// lock, and changes only the requested field. Flexible status editing must not
/// weaken serialization or restore an older status snapshot.
#[test]
fn concurrent_explicit_edit_preserves_the_newer_status() {
    use std::sync::mpsc::sync_channel;
    use std::time::Duration;

    let (_root, runtime) = test_runtime();
    let task = add_proposed_task(&runtime, "Guard under contention");

    let (locked_tx, locked_rx) = sync_channel::<()>(0);
    let (contender_tx, contender_rx) = sync_channel::<()>(0);

    // Contend through the same store-owned lock used by task updates.
    let holder_runtime = &runtime;
    let holder_id = task.id.clone();
    let contended = std::thread::scope(|scope| {
        scope.spawn(move || {
            holder_runtime
                .stores()
                .tasks()
                .with_task_write_lock(&holder_id, &mut || {
                    locked_tx.send(()).expect("announce the held lock");
                    contender_rx.recv().expect("await the contending update");
                    // Long enough that an update which reads before locking has
                    // certainly taken its stale snapshot by now.
                    std::thread::sleep(Duration::from_millis(250));
                    holder_runtime
                        .archive_task(&holder_id)
                        .expect("archive under the lock");
                    Ok(())
                })
                .expect("hold the task lock");
        });

        locked_rx.recv().expect("await the held lock");
        contender_tx
            .send(())
            .expect("announce the contending update");
        runtime.update_task(
            &task.id,
            TaskUpdateParams {
                title: Some("renamed by the loser of the race".to_string()),
                ..Default::default()
            },
        )
    });

    let updated = contended.expect("explicit metadata edit remains valid");
    assert_eq!(updated.status, TaskStatus::Archived);
    let reread = runtime.get_task(&task.id).expect("task still readable");
    assert_eq!(reread.status, TaskStatus::Archived);
    assert_eq!(
        reread.title, "renamed by the loser of the race",
        "the serialized edit must not restore its earlier status snapshot"
    );
}
