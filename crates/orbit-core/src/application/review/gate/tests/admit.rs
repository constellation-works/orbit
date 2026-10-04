//! Admission budgets across resumes, fresh delivery runs and dead runs, and
//! the re-review a completion rebase asks for.

use std::fs;

use chrono::{Duration, Utc};
use orbit_automation::review::{combined_task_meaning_digest, task_meaning_digest};
use orbit_engine::{DispatchError, RuntimeHost};
use orbit_store::contracts::ReviewReserveRequest;
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::{
    JobRunState, ReviewAttemptState, ReviewBudget, ReviewReservation, ReviewVerdict,
};
use serde_json::json;

use crate::application::review::lineage_key;

use super::support::{BEFORE_PR, gated_fixture, git};

#[test]
fn a_fresh_delivery_run_gets_a_full_budget_while_a_resume_shares_it() {
    let gated = gated_fixture(&format!("{BEFORE_PR}review_reviewer_starts = 1\n"));
    let first = gated.admit().expect("admit");
    gated.release(&gated.run_id, &first);

    let resumed = gated.resume(&gated.run_id);
    let error = gated
        .admit_in(&resumed)
        .expect_err("a resume shares the lineage whose only start is spent");
    assert!(
        matches!(error, DispatchError::DeterministicActionRefused { .. }),
        "{error}"
    );
    assert!(
        error.to_string().contains("review_starts_exhausted"),
        "{error}"
    );

    let fresh = gated.fresh_run();
    let admission = gated
        .admit_in(&fresh)
        .expect("a re-admitted task's fresh delivery run has a usable budget");
    assert_eq!(admission["decision"], "admitted");
    assert_eq!(admission["attempt_index"], 1);
    assert_ne!(admission["lineage_key"], first["lineage_key"]);
}

#[test]
fn an_attempt_a_killed_run_left_open_is_released_with_only_its_runtime_charged() {
    let gated = gated_fixture(BEFORE_PR);
    let runtime = &gated.fixture.runtime;
    let started = Utc::now() - Duration::hours(3);
    gated.start(&gated.run_id, started);
    let task = runtime.get_task(&gated.task_id).expect("task");
    let combined = combined_task_meaning_digest(&[(
        task.id.to_string(),
        task_meaning_digest(&task).expect("digest"),
    )])
    .expect("combined");
    let workspace_id = runtime.workspace_id().expect("workspace");
    let lineage = lineage_key(
        &workspace_id,
        std::slice::from_ref(&gated.task_id),
        "main",
        &gated.run_id,
    );
    let candidate = SourceRevision {
        commit: gated.implementation_sha.clone(),
        tree: git(&gated.fixture.repo, &["rev-parse", "HEAD^{tree}"]),
    };
    let ReviewReservation::Reserved { attempt } = runtime
        .review_store()
        .expect("store")
        .review_reserve(
            &workspace_id,
            &ReviewReserveRequest {
                lineage_key: &lineage,
                task_ids: std::slice::from_ref(&gated.task_id),
                run_id: &gated.run_id,
                task_meaning_digest: &combined,
                candidate: &candidate,
                budget: ReviewBudget::default(),
                now: started,
            },
        )
        .expect("reserve in the first run")
        .0
    else {
        panic!("reserve the first run's start");
    };
    RuntimeHost::finalize_job_run(
        runtime,
        &gated.run_id,
        JobRunState::Failed,
        started + Duration::minutes(5),
        None,
    )
    .expect("the first run died five minutes into its review");

    let resumed = gated.resume(&gated.run_id);
    let admission = gated.admit_in(&resumed).expect("admit after the dead run");
    assert_eq!(admission["decision"], "admitted");
    assert_eq!(admission["lineage_key"], lineage.as_str());
    let ledger = gated.ledger(&admission);
    assert_eq!(ledger.attempts.len(), 2);
    let abandoned = &ledger.attempts[0];
    assert_eq!(abandoned.attempt_id, attempt.attempt_id);
    assert_eq!(
        abandoned.state,
        ReviewAttemptState::Settled {
            verdict: ReviewVerdict::Incomplete
        }
    );
    assert_eq!(
        abandoned.elapsed_seconds,
        Some(300),
        "charged until its run ended, not for the hours no run worked it"
    );
    assert_eq!(
        ledger.open_attempt().map(|open| open.run_id.as_str()),
        Some(resumed.as_str())
    );
}

#[test]
fn a_re_review_applies_only_after_completion_recorded_a_rebase_and_pins_its_base() {
    let gated = gated_fixture(BEFORE_PR);
    let repo = &gated.fixture.repo;

    let review_only = gated.re_admit("review").expect("a review-only run");
    assert_eq!(review_only["applies"], false);
    assert_eq!(review_only["reason"], "re_review_not_required");

    let error = gated
        .re_admit("done")
        .expect_err("a completing run with no completion checkpoint fails closed");
    assert!(
        matches!(error, DispatchError::DeterministicActionFailed { .. }),
        "{error}"
    );

    gated.record_completion(json!({ "phase": "complete", "re_review_required": false }));
    let merged = gated.re_admit("done").expect("a completion that merged");
    assert_eq!(merged["applies"], false);
    assert_eq!(merged["reason"], "re_review_not_required");

    git(repo, &["checkout", "main"]);
    fs::write(repo.join("other.txt"), "the base moved\n").expect("advance base");
    git(repo, &["add", "other.txt"]);
    git(repo, &["commit", "-m", "chore: advance the base"]);
    let base_sha = git(repo, &["rev-parse", "HEAD"]);
    git(repo, &["checkout", &format!("orbit/{}", gated.task_id)]);
    git(repo, &["rebase", "main"]);
    let head_sha = git(repo, &["rev-parse", "HEAD"]);
    gated.record_completion(json!({
        "phase": "complete",
        "re_review_required": true,
        "rebased": { "head_sha": head_sha, "base_sha": base_sha },
    }));
    let admission = gated.re_admit("done").expect("re-review the rebased head");
    assert_eq!(admission["applies"], true);
    assert_eq!(admission["head_sha"], head_sha.as_str());
    assert_eq!(admission["base_sha"], base_sha.as_str());
}
