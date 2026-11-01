//! [F2026-10-694] A frozen-batch review whose executor exits early with
//! incomplete coverage evidence: its run fails, the task blocks through
//! `workflow_run_failed`, and delivery settlement must claim the same frozen
//! batch for attempt 2 instead of holding it admitted forever. The second
//! attempt's complete evidence then settles coverage. A run that ends
//! `timeout` or `cancelled` blocks the task through the same event but keeps
//! its own recovery, so settlement leaves its admitted attempt in place.

use super::*;

use orbit_automation::delivery::input_digest;
use orbit_core::application::automation::evaluate_auto_task;
use orbit_engine::TaskAutomationUpdate;
use orbit_store::TaskReservationReleaseReason;
use orbit_types::workflow::automation::{
    BatchAttempt, BatchState, COVERAGE_ARTIFACT, CoverageBatch, Delivery, DeliveryVerdict,
    ExaminationCheck, SourceRevision, evidence_template,
};

const NAME: &str = "frozen-review";

fn definition(workspace: &Workspace) -> orbit_types::workflow::AutoTaskDefinition {
    workspace
        .runtime
        .run_tool(
            "orbit.auto_task.add",
            json!({
                "name": NAME,
                "schedule": {"deliveries_landed": {
                    "branch": "main", "owner_machine": "fixture-machine", "threshold": 1,
                    "max_wait_minutes": 10, "coverage": "landed_code_review_v1", "retries": 1,
                }},
                "template": {"title": "Review landed deliveries"},
            }),
        )
        .unwrap();
    workspace.runtime.auto_task_toggle(NAME, true).unwrap();
    workspace.runtime.auto_task_show(NAME).unwrap().unwrap()
}

fn state(workspace: &Workspace) -> AutomationState {
    let consumer = consumer_key(&workspace.runtime, "auto-task", NAME).unwrap();
    workspace
        .runtime
        .automation_store()
        .unwrap()
        .automation_state(&consumer)
        .unwrap()
        .unwrap()
}

/// Submit `evidence` as `task`'s coverage from executor run `run_id`, through
/// the artifact store that records the evidence-authority witness.
fn put_coverage(
    workspace: &Workspace,
    task: &str,
    run_id: &str,
    evidence: &orbit_types::workflow::automation::CoverageEvidence,
) {
    use orbit_store::contracts::TaskArtifactUpdateParams;
    use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
    use orbit_types::task::TaskArtifact;

    let registry =
        TaskRegistryStore::open(&task_registry_path(&workspace.runtime.global_root())).unwrap();
    let stores = orbit_store::compose::workspace_coordinated_backends(
        registry,
        workspace.runtime.workspace_id().unwrap(),
        workspace.runtime.sqlite_store().unwrap(),
    )
    .unwrap()
    .task;
    stores
        .artifact
        .upsert_task_artifacts(
            task,
            TaskArtifactUpdateParams {
                actor: "codex".into(),
                owner_run_id: Some(run_id.into()),
                upsert_artifacts: vec![TaskArtifact::from_text(
                    COVERAGE_ARTIFACT,
                    serde_json::to_string(evidence).unwrap(),
                )],
                ..Default::default()
            },
        )
        .unwrap();
}

/// Bind `task` to a fresh executor run naming it, as worktree setup does.
fn executor_run(workspace: &Workspace, task: &str) -> String {
    executor_run_owned_by(workspace, task, std::process::id())
}

/// A process id that no longer names a live process.
fn exited_pid() -> u32 {
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

fn executor_run_owned_by(workspace: &Workspace, task: &str, pid: u32) -> String {
    let run = workspace
        .jobs
        .insert_job_run(
            "task_pr_pipeline",
            1,
            Utc::now(),
            Some(json!({"task_id": task})),
            None,
        )
        .unwrap();
    workspace
        .jobs
        .mark_job_run_running(&run.run_id, Utc::now(), pid)
        .unwrap();
    workspace
        .runtime
        .apply_task_automation_update(
            task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::InProgress),
                job_run_id: Some(run.run_id.clone()),
                execution_summary: Some(
                    "Examined the frozen delivery and recorded coverage evidence.".into(),
                ),
                ..TaskAutomationUpdate::default()
            },
        )
        .unwrap();
    run.run_id
}

/// An admitted frozen review whose executor, owned by process `pid`,
/// submitted incomplete evidence from a run that is still running.
struct IncompleteReview {
    now: chrono::DateTime<Utc>,
    batch: CoverageBatch,
    review: String,
    run_id: String,
}

fn admit_incomplete_review(
    workspace: &Workspace,
    definition: &orbit_types::workflow::AutoTaskDefinition,
    pid: u32,
) -> IncompleteReview {
    let now = Utc::now();
    let baseline = evaluate_auto_task(&workspace.runtime, definition, false, now).unwrap();
    assert_eq!(baseline.reason, "baselined", "{baseline:?}");
    let base = baseline.state.unwrap();

    workspace.commit_file("fixture.txt", "landed change\n", "landed change");
    let after = SourceRevision {
        commit: workspace.object("HEAD"),
        tree: workspace.object("HEAD^{tree}"),
    };
    let delivery = Delivery {
        key: "fixture-landing".into(),
        repository: base.repository.clone(),
        branch: base.branch.clone(),
        before: base.baseline.clone(),
        after: after.clone(),
        commits: vec![after.commit.clone()],
        task_ids: vec![],
        unattributed: Some("no_landing_task".into()),
        evidence_reference: "fixture-landing".into(),
        evidence_digest: digest(b"fixture-landing"),
        landed_at: now,
    };
    let batch = CoverageBatch {
        schema_version: 1,
        id: digest(b"frozen review continuation"),
        consumer: base.consumer.clone(),
        epoch: base.epoch.clone(),
        repository: base.repository.clone(),
        branch: base.branch.clone(),
        coverage: orbit_types::workflow::automation::CoverageClass::LandedCodeReviewV1,
        from_exclusive: base.baseline.clone(),
        through_inclusive: after.clone(),
        commits: vec![after.commit.clone()],
        deliveries: vec![delivery.clone()],
        exclusions: vec![],
        created_at: now,
        max_attempts: 2,
        retry_until: now + Duration::hours(1),
    };
    let review = workspace.task("review the frozen batch");
    let attempt = BatchAttempt {
        input_digest: input_digest(&batch).unwrap(),
        action_key: format!("automation:{}:1", batch.id),
        batch: batch.clone(),
        attempt: 1,
        action_id: Some(review.id.clone()),
        state: BatchState::Admitted,
        reason: None,
        retry_after: None,
        reissue: None,
    };
    let mut admitted = base.clone();
    admitted.generation += 1;
    admitted.observed = after.clone();
    admitted.pending_commits = vec![after.commit.clone()];
    admitted.pending = vec![delivery];
    admitted.active = Some(attempt.clone());
    let store = workspace.runtime.automation_store().unwrap();
    assert!(store.automation_commit(&base, &admitted, None).unwrap());

    let run_id = executor_run_owned_by(workspace, &review.id, pid);
    put_coverage(workspace, &review.id, &run_id, &evidence_template(&attempt));
    IncompleteReview {
        now,
        batch,
        review: review.id,
        run_id,
    }
}

/// Terminalize the review's run in `state`. The task blocks through the run's
/// `workflow_run_failed` event whatever the state.
fn end_run(workspace: &Workspace, review: &IncompleteReview, state: JobRunState) {
    workspace
        .runtime
        .finalize_job_run_with_reservation_cleanup(
            &review.run_id,
            state,
            Utc::now(),
            Some(1),
            TaskReservationReleaseReason::RunTerminal,
        )
        .unwrap();
    let blocked = workspace.runtime.get_task(&review.review).unwrap();
    assert_eq!(blocked.status, TaskStatus::Blocked, "{blocked:?}");
    assert!(
        workspace
            .runtime
            .get_task_history(&review.review)
            .unwrap()
            .iter()
            .any(|entry| entry.event == "workflow_run_failed"),
        "the block is the run's"
    );
}

/// Settlement leaves the attempt-1 action admitted on the frozen batch.
fn assert_attempt_one_stays_admitted(
    workspace: &Workspace,
    definition: &orbit_types::workflow::AutoTaskDefinition,
    review: &IncompleteReview,
) {
    let settled = evaluate_auto_task(
        &workspace.runtime,
        definition,
        false,
        review.now + Duration::minutes(1),
    )
    .unwrap();
    let active = settled
        .state
        .as_ref()
        .and_then(|state| state.active.clone())
        .expect("the batch stays owed");
    assert_eq!(active.attempt, 1, "{active:?}");
    assert_eq!(active.state, BatchState::Admitted, "{active:?}");
    assert_eq!(active.action_id.as_deref(), Some(review.review.as_str()));
    assert_eq!(active.batch, review.batch);
}

fn an_incomplete_frozen_review_is_retried_on_the_same_batch_and_can_settle() {
    let workspace = Workspace::new();
    let definition = definition(&workspace);
    let incomplete = admit_incomplete_review(&workspace, &definition, std::process::id());
    let now = incomplete.now;
    let batch = incomplete.batch.clone();
    let after = batch.through_inclusive.clone();
    let store = workspace.runtime.automation_store().unwrap();
    // The executor's run fails: the task blocks through the real
    // run-terminalization path.
    end_run(&workspace, &incomplete, JobRunState::Failed);
    let review = workspace.runtime.get_task(&incomplete.review).unwrap();

    // Settlement claims attempt 2 over the same frozen batch.
    let settled = evaluate_auto_task(
        &workspace.runtime,
        &definition,
        false,
        now + Duration::minutes(1),
    )
    .unwrap();
    let active = settled
        .state
        .as_ref()
        .and_then(|state| state.active.clone())
        .expect("the batch stays owed under a retry");
    assert_eq!(active.attempt, 2, "{active:?}");
    assert_eq!(active.state, BatchState::Claimed, "{active:?}");
    assert_eq!(active.action_id, None, "{active:?}");
    assert_eq!(active.batch, batch, "the same frozen batch is retried");
    assert!(
        active
            .reason
            .as_deref()
            .is_some_and(|reason| reason.ends_with("incomplete_examination")),
        "{active:?}"
    );

    // Past the retry backoff the same batch mints its second action, whose
    // complete evidence settles coverage.
    evaluate_auto_task(
        &workspace.runtime,
        &definition,
        false,
        now + Duration::minutes(7),
    )
    .unwrap();
    let second = state(&workspace).active.expect("attempt 2 admitted");
    assert_eq!(second.attempt, 2, "{second:?}");
    assert_eq!(second.state, BatchState::Admitted, "{second:?}");
    let second_task = second.action_id.clone().expect("attempt 2 task");
    assert_ne!(second_task, review.id);

    let second_run = executor_run(&workspace, &second_task);
    let mut complete = evidence_template(&second);
    complete.examination_complete = true;
    complete.checks = vec![ExaminationCheck {
        subject: "fixture.txt".into(),
        method: "read the landed diff".into(),
        observation: "the change is a single fixture file".into(),
    }];
    complete.delivery_examinations[0].examined_paths = vec!["fixture.txt".into()];
    complete.delivery_examinations[0].verdict = DeliveryVerdict::Clean;
    complete.delivery_examinations[0].rationale =
        "The landed change adds one fixture file and touches no behaviour.".into();
    put_coverage(&workspace, &second_task, &second_run, &complete);

    evaluate_auto_task(
        &workspace.runtime,
        &definition,
        false,
        now + Duration::minutes(8),
    )
    .unwrap();
    let done = state(&workspace);
    assert!(done.active.is_none(), "{done:?}");
    assert_eq!(done.covered, after, "{done:?}");
    let receipts = store.automation_receipts(&done.consumer, 10).unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].action_id, second_task);
}

/// A timed-out run is resumed, which re-admits the same task; a retry minted
/// beside it would review the batch twice.
fn a_timed_out_frozen_review_keeps_its_admitted_attempt() {
    let workspace = Workspace::new();
    let definition = definition(&workspace);
    let review = admit_incomplete_review(&workspace, &definition, std::process::id());
    end_run(&workspace, &review, JobRunState::Timeout);
    assert_attempt_one_stays_admitted(&workspace, &definition, &review);
}

/// Cancellation under the default blocking policy is an operator's decision,
/// not a stop that earns a retry.
fn a_cancelled_frozen_review_keeps_its_admitted_attempt() {
    let workspace = Workspace::new();
    let definition = definition(&workspace);
    let review = admit_incomplete_review(&workspace, &definition, std::process::id());
    end_run(&workspace, &review, JobRunState::Cancelled);
    assert_attempt_one_stays_admitted(&workspace, &definition, &review);
}

/// An operator's later block supersedes the failure's block, even though the
/// run is `failed` and its owner is gone.
fn an_operator_block_after_a_failed_frozen_review_is_not_that_failures_stop() {
    let workspace = Workspace::new();
    let definition = definition(&workspace);
    let review = admit_incomplete_review(&workspace, &definition, exited_pid());
    end_run(&workspace, &review, JobRunState::Failed);
    for (status, event) in [
        (TaskStatus::Backlog, "operator_requeue"),
        (TaskStatus::Blocked, "operator_block"),
    ] {
        workspace
            .runtime
            .apply_task_automation_update(
                &review.review,
                TaskAutomationUpdate {
                    status: Some(status),
                    status_event: Some(event.into()),
                    status_note: Some("held for a decision".into()),
                    ..TaskAutomationUpdate::default()
                },
            )
            .unwrap();
    }
    let history = workspace.runtime.get_task_history(&review.review).unwrap();
    assert_eq!(
        history
            .iter()
            .rev()
            .find(|entry| entry.to_status.is_some())
            .map(|entry| entry.event.as_str()),
        Some("operator_block"),
        "{history:?}"
    );
    assert_attempt_one_stays_admitted(&workspace, &definition, &review);
}

crate::scenario_table!(
    frozen_review_continuation_scenarios,
    an_incomplete_frozen_review_is_retried_on_the_same_batch_and_can_settle,
    a_timed_out_frozen_review_keeps_its_admitted_attempt,
    a_cancelled_frozen_review_keeps_its_admitted_attempt,
    an_operator_block_after_a_failed_frozen_review_is_not_that_failures_stop,
);
