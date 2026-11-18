//! An incomplete before-landing review left its reviewer's commit on the
//! published head; the failure handoff pushed it and kept the PR, and the run
//! ended held for a review decision. An operator requeue resumes that exact
//! PR at the attested reviewer head, so the next run validates and reviews it
//! afresh. A moved remote or local head, a receipt that does not attest the
//! push, a changed specification or a live owner stays refused with nothing
//! written, and a repeated requeue starts nothing new.
use super::escalated_publication::green_forge;
use super::failed_publication::escalate;
use super::handoff_requeue::{
    assert_next_run_resumes_the_published_candidate, refused, requeue, status,
};
use super::published_owner::save_run;
use super::*;
use std::path::Path;

const SETTLE: &str = "landing_review_gate_settle";

/// A receipt case: its name and how it rewrites the handoff receipt.
type ReceiptCase<'a> = (&'a str, &'a dyn Fn(&mut Value));

/// The incident's run: published at P, then the before-landing review
/// committed a correction R and settled `incomplete`; the real failure
/// handoff pushed R onto P and kept PR 17. Returns the fixture with
/// `f.head` = R, the checkout and P.
fn held_reviewer_push_fixture() -> (Fixture, PathBuf, String) {
    let mut f = Fixture::new();
    let path = f
        ._root
        .path()
        .join("global/resources/jobs/task_pr_pipeline.yaml");
    let yaml = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        yaml.replace(
            "  task_delivery: {}",
            "  task_delivery: {}\n  failure_activity: pr_failure_handoff",
        ) + &format!("    - {{id: {SETTLE}, target: 'activity:review_gate_settle'}}\n"),
    )
    .unwrap();
    let checkout = published_owner::publication(&mut f);
    escalate(&f, &f.source.run_id);
    let published = f.head.clone();
    std::fs::write(checkout.join("review.txt"), "reviewer correction\n").unwrap();
    git(&checkout, &["add", "review.txt"]);
    git(
        &checkout,
        &["commit", "-qm", "review: correct the HTTP test"],
    );
    let reviewed = git(&checkout, &["rev-parse", "HEAD"]);
    let output = f.action(
        "pr_failure_handoff",
        json!({"run_id":f.source.run_id,
        "failed_step_id":SETTLE,"activity_name":"review_gate_settle","error_code":"execution",
        "error_message":"review_gate_blocked: verdict incomplete (missing timing evidence)",
        "job_input":f.source.input,"pipeline":f.state.pipeline}),
    );
    assert_eq!(output["decision"], "landing_review_failure", "{output}");
    assert_eq!(output["reviewer_push"]["local_sha"], reviewed, "{output}");
    assert_eq!(output["reviewer_push"]["published_head_sha"], published);
    let remote = git(&f.repo, &["ls-remote", "origin", &f.branch]);
    assert!(
        remote.starts_with(&reviewed),
        "the handoff pushed R: {remote}"
    );
    let mut answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    answer["headRefOid"] = json!(reviewed);
    std::fs::write(&f.answer, answer.to_string()).unwrap();
    let mut state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    state.record_failure_activity("pr_failure_handoff".into(), SETTLE.into(), output);
    f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
    f.state = state;
    let mut run = f.runtime.show_job_run_observed(&f.source.run_id).unwrap();
    run.state = JobRunState::Held;
    run.pid = None;
    run.pid_start_time = None;
    run.finished_at = Some(Utc::now());
    save_run(&f, &run);
    assert!(
        f.runtime
            .complete_job_run_step(
                &run.run_id,
                &orbit_store::contracts::JobRunStepParams {
                    step_index: 40,
                    target_type: orbit_types::workflow::JobTargetType::Activity,
                    target_id: SETTLE.into(),
                    state: JobRunState::Held,
                    started_at: Utc::now(),
                    finished_at: Utc::now(),
                    duration_ms: Some(1),
                    exit_code: Some(0),
                    lock_wait_ms: None,
                    agent_response_json: None,
                    error_code: Some(
                        orbit_types::workflow::REVIEW_DECISION_PENDING_ERROR_CODE.into(),
                    ),
                    error_message: Some(format!(
                        "before-landing review did not approve; {}",
                        orbit_types::workflow::REVIEW_LANDING_DECISION_PENDING
                    )),
                },
            )
            .unwrap()
    );
    f.source = f.runtime.show_job_run_observed(&run.run_id).unwrap();
    f.head = reviewed;
    green_forge(&mut f);
    (f, checkout, published)
}

fn rewrite_receipt(f: &Fixture, edit: impl FnOnce(&mut Value)) -> Value {
    let mut state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    let original = serde_json::to_value(&state).unwrap();
    edit(&mut state.failure_activity_checkpoint.as_mut().unwrap().output);
    f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
    original
}

fn restore_receipt(f: &Fixture, original: Value) {
    f.runtime
        .write_run_state(&f.source.run_id, &serde_json::from_value(original).unwrap())
        .unwrap();
}

/// Move `branch` in `repo` to `sha` (local ref or, through a push, the remote).
fn force_branch(repo: &Path, branch: &str, sha: &str, remote: bool) {
    if remote {
        git(
            repo,
            &[
                "push",
                "-qf",
                "origin",
                &format!("{sha}:refs/heads/{branch}"),
            ],
        );
    } else {
        git(repo, &["update-ref", &format!("refs/heads/{branch}"), sha]);
    }
}

/// The incident: the requeue refused 'retained local branch moved' because it
/// bound the PR to P while the attested reviewer push had carried it to R.
fn a_held_incomplete_review_requeue_resumes_the_attested_reviewer_head() {
    let (f, _, published) = held_reviewer_push_fixture();
    assert_eq!(status(&f), TaskStatus::Blocked);
    let answer = requeue(&f, false).unwrap();
    assert_eq!(answer["status"], "backlog", "{answer}");
    assert_eq!(answer["resumes"]["pull_request"], "17");
    assert_eq!(answer["resumes"]["head_sha"], f.head);
    assert_ne!(f.head, published);
    // A repeated requeue starts nothing and writes no second binding.
    assert!(requeue(&f, false).is_err());
    let preserved = f
        .runtime
        .get_task_history(&f.task)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.event == "published_owner_requeued")
        .count();
    assert_eq!(preserved, 1);
    assert_next_run_resumes_the_published_candidate(&f);
}

/// Every unproven neighbour stays refused with nothing written, and the
/// original then still requeues, so each refusal was its own case's.
fn unattested_or_moved_reviewer_heads_stay_refused() {
    let (f, checkout, published) = held_reviewer_push_fixture();
    let head = f.head.clone();
    let branch = f.branch.clone();
    // Changed local head: the retained branch is no longer the attested R.
    force_branch(&f.repo, &branch, &published, false);
    assert!(refused(&f, false, "local head").contains("moved"));
    force_branch(&f.repo, &branch, &head, false);
    // Changed remote head.
    force_branch(&checkout, &branch, &published, true);
    assert!(refused(&f, false, "remote head").contains("drifted"));
    force_branch(&checkout, &branch, &head, true);
    // Missing or conflicting attestation.
    let cases: [ReceiptCase; 4] = [
        ("missing", &|o| o["reviewer_push"] = Value::Null),
        ("other lease", &|o| {
            o["reviewer_push"]["published_head_sha"] = json!(head)
        }),
        ("other branch", &|o| {
            o["reviewer_push"]["branch"] = json!("elsewhere")
        }),
        ("rewrite", &|o| {
            o["reviewer_push"]["decision"] = json!("performed_force_with_lease")
        }),
    ];
    for (case, edit) in cases {
        let original = rewrite_receipt(&f, edit);
        let error = refused(&f, false, case);
        assert!(
            error.contains("moved") || error.contains("attestation disagrees"),
            "{case}: {error}"
        );
        restore_receipt(&f, original);
    }
    // An active owner.
    let stopped = f.runtime.show_job_run_observed(&f.source.run_id).unwrap();
    let mut live = stopped.clone();
    live.state = JobRunState::Running;
    live.pid = Some(std::process::id());
    live.finished_at = None;
    save_run(&f, &live);
    refused(&f, false, "live owner");
    save_run(&f, &stopped);
    let answer = requeue(&f, false).unwrap();
    assert_eq!(answer["resumes"]["head_sha"], head, "{answer}");
}

/// Criteria edited after the review are refused before anything moves.
fn a_changed_specification_refuses_the_reviewer_head() {
    let (f, _, _) = held_reviewer_push_fixture();
    f.runtime
        .update_task_as_human(
            &f.task,
            orbit_core::application::task::TaskUpdateParams {
                acceptance_criteria: Some(vec!["A different guarantee".into()]),
                ..Default::default()
            },
            "human".into(),
        )
        .unwrap();
    let error = refused(&f, false, "changed specification");
    assert!(error.contains("specification changed"), "{error}");
}

crate::scenario_table!(
    reviewer_push_scenarios,
    a_held_incomplete_review_requeue_resumes_the_attested_reviewer_head,
    unattested_or_moved_reviewer_heads_stay_refused,
    a_changed_specification_refuses_the_reviewer_head
);
