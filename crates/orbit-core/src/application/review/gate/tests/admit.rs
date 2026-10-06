//! Admission budgets across resumes, fresh delivery runs and dead runs, and
//! the re-review a completion rebase asks for.

use std::fs;

use chrono::{Duration, Utc};
use orbit_automation::review::{combined_task_meaning_digest, task_meaning_digest};
use orbit_engine::{DispatchError, RuntimeHost};
use orbit_store::contracts::{ReviewInvocationRecord, ReviewReserveRequest};
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::{
    JobRunState, ReviewAttemptState, ReviewBudget, ReviewReservation, ReviewVerdict,
    ReviewerInvocationEvent,
};
use serde_json::json;

use crate::application::review::lineage_key;

use super::support::{BEFORE_PR, gated_fixture, git};

#[test]
fn each_candidate_gets_one_review_while_a_fresh_delivery_run_starts_over() {
    let gated = gated_fixture(BEFORE_PR);
    let first = gated.admit().expect("admit");
    gated.reviewer_ran(&gated.run_id, &first, 30);
    let runtime = &gated.fixture.runtime;
    runtime
        .review_store()
        .expect("store")
        .review_settle(
            &runtime.workspace_id().expect("workspace"),
            &orbit_store::contracts::ReviewSettlement {
                lineage_key: first["lineage_key"].as_str().expect("lineage"),
                attempt_id: first["attempt_id"].as_str().expect("attempt"),
                verdict: ReviewVerdict::Accept,
                now: Utc::now(),
            },
        )
        .expect("settle with a verdict");

    let resumed = gated.resume(&gated.run_id);
    let error = gated
        .admit_in(&resumed)
        .expect_err("a resume of the same candidate gets no second review");
    assert!(
        matches!(error, DispatchError::DeterministicActionRefused { .. }),
        "{error}"
    );
    assert!(
        error.to_string().contains("review_candidate_reviewed"),
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
fn a_run_that_dies_mid_review_releases_its_attempt_when_it_terminates() {
    let gated = gated_fixture(BEFORE_PR);
    let runtime = &gated.fixture.runtime;
    let started = Utc::now() - Duration::hours(4);
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
    let store = runtime.review_store().expect("store");
    let ReviewReservation::Reserved { attempt } = store
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
    // The reviewer started a minute in under a ten-minute bound; its process
    // died with the run, which was found dead and finalized hours later.
    store
        .review_record_invocation(
            &workspace_id,
            &ReviewInvocationRecord {
                lineage_key: &lineage,
                attempt_id: &attempt.attempt_id,
                run_id: &gated.run_id,
                event: ReviewerInvocationEvent::Started {
                    timeout_seconds: 600,
                },
                now: started + Duration::minutes(1),
            },
        )
        .expect("record the reviewer start");
    RuntimeHost::finalize_job_run(
        runtime,
        &gated.run_id,
        JobRunState::Interrupted,
        started + Duration::hours(3),
        None,
    )
    .expect("finalize the dead run");

    let ledger = store
        .review_ledger(&workspace_id, &lineage)
        .expect("read")
        .expect("ledger");
    assert!(
        ledger.open_attempt().is_none(),
        "terminating the run released its attempt without waiting for another admission"
    );
    let released = &ledger.attempts[0];
    assert_eq!(
        released.state,
        ReviewAttemptState::Settled {
            verdict: ReviewVerdict::Incomplete
        }
    );
    assert_eq!(
        released.elapsed_seconds,
        Some(600),
        "a reviewer that never reported its end is charged up to its own bound, not the hours \
         before the run was found dead"
    );

    let fresh = gated.fresh_run();
    let admission = gated
        .admit_in(&fresh)
        .expect("the re-admitted task's fresh run has a usable budget");
    assert_eq!(admission["decision"], "admitted");
    assert_ne!(admission["lineage_key"], lineage.as_str());
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

#[test]
fn reset_retires_an_exhausted_attempt_and_preserves_the_history_for_a_fresh_start() {
    if crate::application::run_isolated_test(std::any::type_name_of_val(
        &reset_retires_an_exhausted_attempt_and_preserves_the_history_for_a_fresh_start,
    )) {
        return;
    }
    use orbit_store::contracts::{ReviewResetRequest, ReviewSettlement};
    let gated = gated_fixture(BEFORE_PR);
    let first = gated.admit().expect("admit the candidate's review");
    gated.reviewer_ran(&gated.run_id, &first, 8000);
    let runtime = &gated.fixture.runtime;
    let workspace = runtime.workspace_id().unwrap();
    let store = runtime.review_store().unwrap();
    let lineage = first["lineage_key"].as_str().unwrap();
    let attempt = first["attempt_id"].as_str().unwrap();
    let preflight_input = json!({
        "job_run_id": gated.run_id,
        "completed_task_ids": gated.bundle,
        "workspace_path": gated.fixture.repo,
        "base": "main", "preflight": true,
    });
    let preflight = || {
        crate::application::review::review_gate_admit(
            runtime,
            "review_gate_admit",
            &preflight_input,
        )
    };
    let error = preflight().expect_err("exhaustion refuses before implementation");
    assert!(
        matches!(error, DispatchError::DeterministicActionRefused { .. }),
        "{error}"
    );
    assert!(
        error.to_string().contains("orbit task review-reset"),
        "{error}"
    );
    let reset = store
        .review_reset(
            &workspace,
            &ReviewResetRequest {
                lineage_key: lineage,
                task_id: &gated.task_id,
                reason: "Repair obsolete timeout accounting",
                actor: "human:operator",
                budget: None,
                now: Utc::now(),
            },
        )
        .expect("reset with the open attempt still present");
    assert_eq!(reset.attempts.len(), 1);
    assert!(reset.open_attempt().is_none());
    assert_eq!(reset.consumed_seconds, 0);
    assert_eq!(reset.decisions[0].previous_consumption.seconds, 8000);
    assert_eq!(
        reset.decisions[0].reason,
        "Repair obsolete timeout accounting"
    );
    assert_eq!(reset.decisions[0].actor, "human:operator");
    let historical = reset.as_of(attempt).unwrap();
    assert_eq!(historical.consumed().seconds, 8000);
    assert!(historical.decisions.is_empty());
    assert!(
        store
            .review_settle(
                &workspace,
                &ReviewSettlement {
                    lineage_key: lineage,
                    attempt_id: attempt,
                    verdict: ReviewVerdict::Accept,
                    now: Utc::now(),
                }
            )
            .is_err(),
        "late settlement cannot resurrect the retired attempt"
    );
    assert_eq!(preflight().unwrap()["decision"], "preflight_passed");
    assert_eq!(
        store.review_ledger(&workspace, lineage).unwrap().unwrap(),
        reset,
        "preflight reserves no attempt"
    );
    let next = gated.admit().expect("the reset permits another admission");
    assert_eq!(next["attempt_index"], 2);
    assert_ne!(next["attempt_id"], first["attempt_id"]);
    let ledger = gated.ledger(&next);
    assert_eq!(ledger.attempts.len(), 2);
    assert_eq!(ledger.decisions, reset.decisions);
    assert_eq!(
        ledger.remaining_at(Utc::now()).seconds,
        30 * 60,
        "the reset candidate's review starts over with its full minutes"
    );
    assert_eq!(ledger.as_of(attempt).unwrap().consumed().seconds, 8000);
}

/// [ORB-13992] `review.minutes` is the wall-clock limit for one candidate's
/// review: each reviewer invocation is bounded by what the review has left,
/// an unfinished review continues on resume, and once the minutes are spent
/// no reviewer is started again for that candidate.
#[test]
fn review_minutes_bound_each_reviewer_and_refuse_a_retry_once_spent() {
    let gated = gated_fixture(&format!("{BEFORE_PR}minutes = 1\n"));
    let runtime = &gated.fixture.runtime;
    let invoke = |run_id: &str, admission: &serde_json::Value, event| {
        crate::application::review::record_reviewer_invocation(
            runtime,
            &orbit_engine::ReviewerInvocationRequest {
                run_id: run_id.to_string(),
                lineage_key: admission["lineage_key"].as_str().expect("lineage").into(),
                attempt_id: admission["attempt_id"].as_str().expect("attempt").into(),
                event,
            },
        )
        .expect("record reviewer invocation")
    };

    let first = gated.admit().expect("admit");
    assert_eq!(
        invoke(
            &gated.run_id,
            &first,
            ReviewerInvocationEvent::Started {
                timeout_seconds: 3600
            }
        ),
        Some(30),
        "the invocation reserves half the remaining minutes for continuation"
    );
    invoke(
        &gated.run_id,
        &first,
        ReviewerInvocationEvent::Finished {
            runtime_seconds: 45,
        },
    );
    gated.release(&gated.run_id, &first);

    let resumed = gated.resume(&gated.run_id);
    let retry = gated
        .admit_in(&resumed)
        .expect("an unfinished review continues while minutes remain");
    assert_eq!(retry["remaining"]["seconds"], 15);
    assert_eq!(
        invoke(
            &resumed,
            &retry,
            ReviewerInvocationEvent::Started {
                timeout_seconds: 3600
            }
        ),
        Some(7)
    );
    invoke(
        &resumed,
        &retry,
        ReviewerInvocationEvent::Finished {
            runtime_seconds: 15,
        },
    );
    gated.release(&resumed, &retry);

    let error = gated
        .admit_in(&gated.resume(&resumed))
        .expect_err("the candidate's minutes are spent");
    assert!(
        error.to_string().contains("review_minutes_exhausted"),
        "{error}"
    );
}

/// [ORB-13992] A delivery run captures `review.before_pr` at submission:
/// turning it off does not drop the review of a run already in flight, a run
/// submitted afterwards captures it off, and turning it back on does not
/// start reviewing that run either.
#[test]
fn before_pr_is_captured_at_submission_and_an_in_flight_run_keeps_it() {
    let before_pr_off = BEFORE_PR.replace("before_pr = true", "before_pr = false");
    let mut gated = gated_fixture(BEFORE_PR);

    gated.reconfigure(&before_pr_off);
    assert!(
        !gated
            .fixture
            .runtime
            .operation_policy()
            .review_before_pr
            .value
    );
    let in_flight = gated
        .admit()
        .expect("the in-flight run keeps its captured before_pr");
    assert_eq!(in_flight["applies"], true);
    assert_eq!(in_flight["decision"], "admitted");
    assert_eq!(in_flight["timing"], "before-pr");

    let captured_off = gated.fresh_run();
    let off = gated.admit_in(&captured_off).expect("not applicable");
    assert_eq!(off["applies"], false);
    assert_eq!(off["reason"], "review_before_pr_off");
    assert_eq!(off["timing"], "none");

    gated.reconfigure(BEFORE_PR);
    let still_off = gated.admit_in(&captured_off).expect("not applicable");
    assert_eq!(
        still_off["applies"], false,
        "turning before_pr on does not reach a run captured off"
    );
}
