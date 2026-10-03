//! [ORB-13744] The public delivery observation reads only host step
//! checkpoints, refuses foreign ownership, and reports every partial or
//! corrupt record as a typed gap.

use super::*;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::contracts::JobRunStepParams;
use orbit_types::workflow::{
    CommitObservationStatus, DeliveryEvidenceGap, JobRunState, JobTargetType, LandingMethod,
    LandingObservationStatus, PipelineState, RUN_DELIVERY_EVIDENCE_SOURCE, RunDeliveryStatus,
};
use serde_json::{Value, json};

use crate::application::task::TaskAddParams;

use super::super::delivery::DeliveryJobShape;

const BASE: &str = "1111111111111111111111111111111111111111";
const HEAD: &str = "2222222222222222222222222222222222222222";
const LANDED: &str = "3333333333333333333333333333333333333333";
const FORGED: &str = "dddddddddddddddddddddddddddddddddddddddd";
const SECRET: &str = "sk-live-delivery-observation-must-not-echo";

/// A temp-rooted runtime whose global catalog holds the shipped jobs and
/// activities, so step identity comes from the real definitions.
fn delivery_runtime() -> (tempfile::TempDir, OrbitRuntime) {
    let (root, runtime) = test_runtime();
    let global_root = runtime.global_root();
    crate::bootstrap::activity::seed_default_activities(
        &global_root.join("resources/activities"),
        true,
    )
    .expect("seed default activities");
    crate::application::job::seed_default_jobs(&global_root.join("resources/jobs"), true)
        .expect("seed default jobs");
    (root, runtime)
}

fn add_task(runtime: &OrbitRuntime) -> String {
    runtime
        .add_task(TaskAddParams {
            title: "Deliver a change".to_string(),
            description: "Exercise the delivery observation.".to_string(),
            acceptance_criteria: vec!["The change is delivered.".to_string()],
            plan: "Commit and land it.".to_string(),
            ..Default::default()
        })
        .expect("add task")
        .id
        .to_string()
}

fn insert_run(runtime: &OrbitRuntime, job_id: &str, task_ids: &[&str]) -> JobRun {
    runtime
        .stores()
        .jobs()
        .insert_job_run(
            job_id,
            1,
            Utc::now(),
            Some(json!({ "task_ids": task_ids, "prompt": SECRET })),
            None,
        )
        .expect("insert run")
}

fn finish(runtime: &OrbitRuntime, run: &JobRun, state: JobRunState) {
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("mark running");
    runtime
        .stores()
        .jobs()
        .finalize_job_run(&run.run_id, state, Utc::now(), Some(10))
        .expect("finalize run");
}

fn shape(runtime: &OrbitRuntime, job_id: &str) -> DeliveryJobShape {
    let (_, mut job) = runtime
        .load_v2_job_asset_by_name(job_id)
        .expect("load shipped job");
    let catalog = runtime.v2_activity_catalog().expect("activity catalog");
    orbit_engine::resolve_job_catalog_refs_for_execution(&mut job, &catalog)
        .expect("resolve shipped activities");
    DeliveryJobShape::from_job(&job)
}

fn index_of(runtime: &OrbitRuntime, job_id: &str, step_id: &str) -> u32 {
    let (_, job) = runtime
        .load_v2_job_asset_by_name(job_id)
        .expect("load shipped job");
    job.steps
        .iter()
        .position(|step| step.id == step_id)
        .and_then(|index| u32::try_from(index).ok())
        .unwrap_or_else(|| panic!("{job_id} has step {step_id}"))
}

/// Checkpoint one step exactly as the host does: its raw output under its
/// index, and the same value under its id in the pipeline.
fn checkpoint(state: &mut PipelineState, index: u32, step_id: &str, output: Value) {
    state.record_step(index, JobRunState::Success, Some(output.clone()), None);
    state.record_pipeline_output(step_id, output);
}

fn performed_commit(task_id: &str, run_id: &str) -> Value {
    json!({
        "phase": "commit",
        "decision": "performed",
        "committed": true,
        "commit_sha": HEAD,
        "base_sha": BASE,
        "job_run_id": run_id,
        "skipped_no_diff_expected": false,
        "task_id": task_id,
    })
}

fn merged_pr() -> Value {
    json!({
        "phase": "complete",
        "no_diff_expected": false,
        "merge": {
            "merged": true,
            "pr_number": "2978",
            "landed_commit": LANDED,
            "delivery_evidence": { "token": SECRET },
        },
        "completed_task_ids": [],
    })
}

/// A delivery run of `job_id` for one fresh task, with the checkpoints each
/// test writes persisted on `observe`.
struct PrRun {
    runtime_root: tempfile::TempDir,
    runtime: OrbitRuntime,
    task_id: String,
    run: JobRun,
    state: PipelineState,
}

impl PrRun {
    fn new(job_id: &str) -> Self {
        let (runtime_root, runtime) = delivery_runtime();
        let task_id = add_task(&runtime);
        let run = insert_run(&runtime, job_id, &[&task_id]);
        let state = PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({}));
        Self {
            runtime_root,
            runtime,
            task_id,
            run,
            state,
        }
    }

    fn step(&mut self, step_id: &str, output: Value) -> &mut Self {
        let index = index_of(&self.runtime, &self.run.job_id, step_id);
        checkpoint(&mut self.state, index, step_id, output);
        self
    }

    fn commit(&mut self) -> &mut Self {
        let output = performed_commit(&self.task_id, &self.run.run_id);
        self.step("commit", output)
    }

    fn observe(&self) -> Result<orbit_types::workflow::RunDeliveryObservation, OrbitError> {
        self.runtime
            .write_run_state(&self.run.run_id, &self.state)
            .expect("write run state");
        self.runtime
            .observe_run_delivery(&self.run.run_id, &self.task_id)
    }
}

#[test]
fn shipped_delivery_jobs_resolve_their_host_steps_by_definition() {
    let (_root, runtime) = delivery_runtime();

    let pr = shape(&runtime, "task_pr_pipeline");
    let commit = pr.commit.expect("PR pipeline commits through the host");
    assert_eq!(commit.id, "commit");
    assert_eq!(
        commit.index,
        index_of(&runtime, "task_pr_pipeline", "commit")
    );
    assert_eq!(
        pr.landings
            .iter()
            .map(|step| step.id.as_str())
            .collect::<Vec<_>>(),
        ["complete_pr", "complete_no_diff"]
    );

    let local = shape(&runtime, "task_local_pipeline");
    assert_eq!(local.commit.map(|step| step.id), Some("commit".to_string()));
    assert_eq!(
        local
            .landings
            .iter()
            .map(|step| step.id.as_str())
            .collect::<Vec<_>>(),
        ["merge"]
    );

    let claimed = shape(&runtime, "task_claimed_pr_pipeline");
    assert!(claimed.commit.is_some());
    assert!(
        claimed.landings.is_empty(),
        "a claimed run hands off; its owner lands it"
    );
}

#[test]
fn a_merged_pull_request_is_a_landed_delivery_with_host_provenance() {
    let mut fixture = PrRun::new("task_pr_pipeline");
    fixture.commit().step("complete_pr", merged_pr());
    let commit_index = index_of(&fixture.runtime, "task_pr_pipeline", "commit");
    fixture
        .runtime
        .stores()
        .jobs()
        .complete_job_run_step(
            &fixture.run.run_id,
            &JobRunStepParams {
                step_index: commit_index as usize,
                target_type: JobTargetType::Activity,
                target_id: "commit".to_string(),
                started_at: Utc::now(),
                finished_at: Utc::now(),
                duration_ms: Some(5),
                exit_code: None,
                agent_response_json: None,
                state: JobRunState::Success,
                error_code: None,
                error_message: None,
            },
        )
        .expect("record commit step");
    finish(&fixture.runtime, &fixture.run, JobRunState::Success);

    let observed = fixture.observe().expect("observe delivery");

    assert_eq!(observed.delivery_status, RunDeliveryStatus::Landed);
    assert_eq!(observed.task_id, fixture.task_id);
    assert_eq!(observed.run_id, fixture.run.run_id);
    assert_eq!(
        observed.workspace_id,
        fixture.runtime.workspace_id().expect("workspace id")
    );
    assert_eq!(observed.run_state, JobRunState::Success);
    assert!(observed.run_finished_at.is_some());
    assert_eq!(observed.commit.status, CommitObservationStatus::Committed);
    assert_eq!(observed.commit.base_sha.as_deref(), Some(BASE));
    assert_eq!(observed.commit.head_sha.as_deref(), Some(HEAD));
    assert!(observed.commit.observed_at.is_some());
    let provenance = observed.commit.provenance.expect("commit provenance");
    assert_eq!(provenance.source, RUN_DELIVERY_EVIDENCE_SOURCE);
    assert_eq!(provenance.activity, "git_commit");
    assert_eq!(provenance.step_index, commit_index);
    assert_eq!(observed.landing.status, LandingObservationStatus::Merged);
    assert_eq!(observed.landing.method, Some(LandingMethod::PullRequest));
    assert_eq!(observed.landing.landed_commit.as_deref(), Some(LANDED));
    assert_eq!(observed.landing.pr_number, Some(2978));
    drop(fixture.runtime_root);
}

#[test]
fn a_review_only_run_is_committed_but_not_landed() {
    let mut fixture = PrRun::new("task_pr_pipeline");
    fixture
        .commit()
        .step("complete_pr", Value::Null)
        .step("complete_no_diff", Value::Null);
    finish(&fixture.runtime, &fixture.run, JobRunState::Success);

    let observed = fixture.observe().expect("observe delivery");

    assert_eq!(observed.delivery_status, RunDeliveryStatus::Committed);
    assert_eq!(observed.commit.head_sha.as_deref(), Some(HEAD));
    assert_eq!(
        observed.landing.status,
        LandingObservationStatus::NotRequested
    );
    assert_eq!(observed.landing.landed_commit, None);
}

#[test]
fn a_successful_workflow_without_a_commit_checkpoint_is_not_a_delivery() {
    let fixture = PrRun::new("task_pr_pipeline");
    finish(&fixture.runtime, &fixture.run, JobRunState::Success);

    let observed = fixture.observe().expect("observe delivery");

    assert_eq!(observed.delivery_status, RunDeliveryStatus::NotDelivered);
    assert_eq!(observed.commit.status, CommitObservationStatus::NotReached);
    assert_eq!(observed.commit.head_sha, None);
}

#[test]
fn a_merge_record_without_its_commit_checkpoint_is_not_a_landing() {
    let mut fixture = PrRun::new("task_pr_pipeline");
    fixture.step("complete_pr", merged_pr());
    finish(&fixture.runtime, &fixture.run, JobRunState::Success);

    let observed = fixture.observe().expect("observe delivery");

    assert_eq!(observed.delivery_status, RunDeliveryStatus::NotDelivered);
    assert_eq!(
        observed.landing.status,
        LandingObservationStatus::NotReached
    );
    assert_eq!(observed.landing.landed_commit, None);
}

#[test]
fn a_running_run_reports_pending_evidence() {
    let fixture = PrRun::new("task_pr_pipeline");

    let observed = fixture.observe().expect("observe delivery");

    assert_eq!(observed.delivery_status, RunDeliveryStatus::InProgress);
    assert_eq!(observed.commit.status, CommitObservationStatus::Pending);
    assert_eq!(observed.landing.status, LandingObservationStatus::Pending);
}

#[test]
fn a_run_that_failed_after_committing_is_committed_and_unlanded() {
    let mut fixture = PrRun::new("task_pr_pipeline");
    fixture.commit();
    finish(&fixture.runtime, &fixture.run, JobRunState::Failed);

    let observed = fixture.observe().expect("observe delivery");

    assert_eq!(observed.delivery_status, RunDeliveryStatus::Committed);
    assert_eq!(
        observed.landing.status,
        LandingObservationStatus::NotReached
    );
}

#[test]
fn a_terminal_run_without_pipeline_state_is_unavailable_not_successful() {
    let (_root, runtime) = delivery_runtime();
    let task_id = add_task(&runtime);
    let run = insert_run(&runtime, "task_pr_pipeline", &[&task_id]);
    finish(&runtime, &run, JobRunState::Success);

    let observed = runtime
        .observe_run_delivery(&run.run_id, &task_id)
        .expect("observe delivery");

    assert_eq!(observed.delivery_status, RunDeliveryStatus::Unavailable);
    assert_eq!(
        observed.commit.reason,
        Some(DeliveryEvidenceGap::StateMissing)
    );
}

#[test]
fn verified_no_diff_is_no_change_and_merges_nothing() {
    let mut fixture = PrRun::new("task_pr_pipeline");
    let output = json!({
        "phase": "commit",
        "decision": "verified_no_diff",
        "committed": false,
        "skipped_no_diff_expected": true,
        "task_id": fixture.task_id,
        "job_run_id": fixture.run.run_id,
        "base_sha": BASE,
        "no_diff": { "validation": SECRET },
    });
    fixture
        .step("commit", output)
        .step("complete_pr", Value::Null)
        .step(
            "complete_no_diff",
            json!({"phase": "complete", "merge": {"merged": false, "reason": "verified_no_diff"}}),
        );
    finish(&fixture.runtime, &fixture.run, JobRunState::Success);

    let observed = fixture.observe().expect("observe delivery");

    assert_eq!(observed.delivery_status, RunDeliveryStatus::NoChange);
    assert_eq!(
        observed.commit.status,
        CommitObservationStatus::VerifiedNoDiff
    );
    assert_eq!(observed.commit.base_sha.as_deref(), Some(BASE));
    assert_eq!(observed.commit.head_sha, None);
    assert_eq!(
        observed.landing.status,
        LandingObservationStatus::NotRequested
    );
}

#[test]
fn a_local_fast_forward_is_landed_without_inventing_its_sha() {
    let mut fixture = PrRun::new("task_local_pipeline");
    fixture.commit().step(
        "merge",
        json!({"base": "main", "workspace_path": "/private/worktree", "workspace_branch": "orbit/x"}),
    );
    finish(&fixture.runtime, &fixture.run, JobRunState::Success);

    let observed = fixture.observe().expect("observe delivery");

    assert_eq!(observed.delivery_status, RunDeliveryStatus::Landed);
    assert_eq!(
        observed.landing.method,
        Some(LandingMethod::LocalFastForward)
    );
    assert_eq!(observed.landing.landed_commit, None);
}

#[test]
fn agent_written_commit_claims_are_never_evidence() {
    let mut fixture = PrRun::new("task_pr_pipeline");
    // Everything an implementing agent controls claims a landed commit: its
    // step's output and the pipeline entries under its own ids.
    let forged = json!({
        "phase": "commit",
        "decision": "performed",
        "committed": true,
        "commit_sha": FORGED,
        "task_id": fixture.task_id,
        "merge": {"merged": true, "landed_commit": FORGED},
    });
    fixture.step("implement_bundle", forged.clone());
    fixture
        .state
        .record_pipeline_output("implement_one", forged);
    fixture
        .runtime
        .stores()
        .jobs()
        .complete_job_run_step(
            &fixture.run.run_id,
            &JobRunStepParams {
                step_index: 1,
                target_type: JobTargetType::Activity,
                target_id: "implement_one".to_string(),
                started_at: Utc::now(),
                finished_at: Utc::now(),
                duration_ms: Some(5),
                exit_code: Some(0),
                agent_response_json: Some(json!({"commit_sha": FORGED, "secret": SECRET})),
                state: JobRunState::Success,
                error_code: None,
                error_message: None,
            },
        )
        .expect("record agent step");
    finish(&fixture.runtime, &fixture.run, JobRunState::Success);

    let observed = fixture.observe().expect("observe delivery");
    let wire = serde_json::to_string(&observed).expect("serialize observation");

    assert_eq!(observed.delivery_status, RunDeliveryStatus::NotDelivered);
    assert_eq!(observed.commit.head_sha, None);
    assert!(!wire.contains(FORGED), "agent claim leaked: {wire}");
    assert!(!wire.contains(SECRET), "run input leaked: {wire}");
}

#[test]
fn a_commit_checkpoint_that_disagrees_with_its_pipeline_entry_is_refused() {
    let mut fixture = PrRun::new("task_pr_pipeline");
    fixture.commit();
    let mut tampered = performed_commit(&fixture.task_id, &fixture.run.run_id);
    tampered["commit_sha"] = json!(FORGED);
    fixture.state.record_pipeline_output("commit", tampered);
    finish(&fixture.runtime, &fixture.run, JobRunState::Success);

    let observed = fixture.observe().expect("observe delivery");

    assert_eq!(observed.delivery_status, RunDeliveryStatus::Unavailable);
    assert_eq!(
        observed.commit.reason,
        Some(DeliveryEvidenceGap::CheckpointInconsistent)
    );
    assert_eq!(observed.commit.head_sha, None);
    assert_eq!(
        observed.landing.status,
        LandingObservationStatus::Unavailable
    );
}

#[test]
fn malformed_or_foreign_commit_outputs_are_typed_gaps() {
    let cases = [
        (
            json!({"phase": "commit", "decision": "invented", "committed": true, "commit_sha": HEAD}),
            DeliveryEvidenceGap::OutputMalformed,
        ),
        (
            json!({"phase": "commit", "decision": "performed", "committed": true, "commit_sha": "HEAD"}),
            DeliveryEvidenceGap::OutputMalformed,
        ),
        (
            json!({"phase": "commit", "decision": "performed", "committed": true}),
            DeliveryEvidenceGap::OutputMalformed,
        ),
        (json!("performed"), DeliveryEvidenceGap::OutputMalformed),
        (
            json!({"phase": "commit", "decision": "performed", "committed": true, "commit_sha": HEAD, "task_id": "ORB-99999"}),
            DeliveryEvidenceGap::OwnershipMismatch,
        ),
    ];
    for (mut output, gap) in cases {
        let mut fixture = PrRun::new("task_pr_pipeline");
        if output.is_object() && output.get("task_id").is_none() {
            output["task_id"] = json!(fixture.task_id);
        }
        fixture.step("commit", output.clone());
        finish(&fixture.runtime, &fixture.run, JobRunState::Success);

        let observed = fixture.observe().expect("observe delivery");

        assert_eq!(
            observed.delivery_status,
            RunDeliveryStatus::Unavailable,
            "{output}"
        );
        assert_eq!(observed.commit.reason, Some(gap), "{output}");
        assert_eq!(observed.commit.head_sha, None, "{output}");
    }
}

#[test]
fn a_commit_checkpoint_naming_another_run_is_refused() {
    let mut fixture = PrRun::new("task_pr_pipeline");
    let mut output = performed_commit(&fixture.task_id, &fixture.run.run_id);
    output["job_run_id"] = json!("jrun-20260101-0000-zz");
    fixture.step("commit", output);
    finish(&fixture.runtime, &fixture.run, JobRunState::Success);

    let observed = fixture.observe().expect("observe delivery");

    assert_eq!(
        observed.commit.reason,
        Some(DeliveryEvidenceGap::OwnershipMismatch)
    );
}

#[test]
fn ownership_is_checked_before_any_evidence_is_read() {
    let (_root, runtime) = delivery_runtime();
    let task_id = add_task(&runtime);
    let other_task = add_task(&runtime);
    let run = insert_run(&runtime, "task_pr_pipeline", &[&task_id]);

    let not_carried = runtime
        .observe_run_delivery(&run.run_id, &other_task)
        .expect_err("a run that did not carry the task must refuse");
    assert!(
        matches!(not_carried, OrbitError::InvalidInput(_)),
        "{not_carried:?}"
    );

    let unknown_run = runtime
        .observe_run_delivery("jrun-20260101-0000-zz", &task_id)
        .expect_err("an unknown run must refuse");
    assert!(
        matches!(unknown_run, OrbitError::NotFound { .. }),
        "{unknown_run:?}"
    );

    let foreign_task = insert_run(&runtime, "task_pr_pipeline", &["ORB-99999"]);
    let missing = runtime
        .observe_run_delivery(&foreign_task.run_id, "ORB-99999")
        .expect_err("a task outside this workspace must refuse");
    assert!(
        matches!(missing, OrbitError::NotFound { .. }),
        "{missing:?}"
    );

    let oversized = "x".repeat(129);
    assert!(matches!(
        runtime.observe_run_delivery(&oversized, &task_id),
        Err(OrbitError::InvalidInput(_))
    ));
}

#[test]
fn another_workspace_cannot_observe_this_workspaces_run() {
    let (_root, runtime) = delivery_runtime();
    let task_id = add_task(&runtime);
    let run = insert_run(&runtime, "task_pr_pipeline", &[&task_id]);
    let (_other_root, other) = delivery_runtime();

    let refused = other
        .observe_run_delivery(&run.run_id, &task_id)
        .expect_err("a foreign workspace's run must not be visible");

    assert!(
        matches!(refused, OrbitError::NotFound { .. }),
        "{refused:?}"
    );
}

#[test]
fn a_run_of_a_non_delivery_job_is_refused() {
    let (_root, runtime) = delivery_runtime();
    let task_id = add_task(&runtime);
    let run = insert_run(&runtime, "task_pilot_pipeline", &[&task_id]);

    let refused = runtime
        .observe_run_delivery(&run.run_id, &task_id)
        .expect_err("a non-delivery run must refuse");

    assert!(
        matches!(refused, OrbitError::InvalidInput(_)),
        "{refused:?}"
    );
}

#[test]
fn an_unnamed_run_defaults_to_the_newest_delivery_run_submitted_with_the_task() {
    let (_root, runtime) = delivery_runtime();
    let task_id = add_task(&runtime);
    let other_task = add_task(&runtime);

    let refused = runtime
        .observe_task_delivery(&task_id, None)
        .expect_err("a task no delivery run carried has nothing to observe");
    assert!(
        matches!(refused, OrbitError::InvalidInput(_)),
        "{refused:?}"
    );

    let delivered = insert_run(&runtime, "task_pr_pipeline", &[&task_id]);
    // Newer, but neither a delivery job nor a run that carried this task.
    insert_run(&runtime, "task_pilot_pipeline", &[&task_id]);
    insert_run(&runtime, "task_pr_pipeline", &[&other_task]);
    let observed = runtime
        .observe_task_delivery(&task_id, None)
        .expect("observe the default run");
    assert_eq!(observed.run_id, delivered.run_id);
    assert_eq!(observed.task_id, task_id);

    let redelivered = insert_run(&runtime, "task_pr_pipeline", &[&task_id]);
    assert_eq!(
        runtime
            .observe_task_delivery(&task_id, None)
            .expect("observe the newest run")
            .run_id,
        redelivered.run_id
    );
    assert_eq!(
        runtime
            .observe_task_delivery(&task_id, Some(&delivered.run_id))
            .expect("a named run is observed as named")
            .run_id,
        delivered.run_id
    );
}
