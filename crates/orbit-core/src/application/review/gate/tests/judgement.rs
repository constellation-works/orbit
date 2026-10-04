//! Judging the reviewer's claims against the lineage's review budget.

use chrono::{Duration, Utc};
use orbit_automation::review::{combined_task_meaning_digest, task_meaning_digest};
use orbit_store::contracts::ReviewReserveRequest;
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::{ReviewBudget, ReviewReservation, ReviewVerdict};

use crate::application::review::lineage_key;

use super::support::{gated_fixture, git, report, write_report};

/// Safety seam: settlement currently emits no history stubs, so its public
/// boundary cannot provoke a history-producing system edit. Guard the
/// shared deterministic writer against false human intervention [ORB-13916].
#[test]
fn system_writes_cannot_borrow_the_operator_for_history() {
    if crate::application::run_isolated_test(std::any::type_name_of_val(
        &system_writes_cannot_borrow_the_operator_for_history,
    )) {
        return;
    }
    let mut gated = gated_fixture("");
    gated.fixture.runtime = gated
        .fixture
        .runtime
        .with_actor(crate::ActorIdentity::human("human:daniel"));
    let runtime = &gated.fixture.runtime;
    let before = runtime.get_task_history(&gated.task_id).expect("history");
    runtime
        .update_task_as_system(
            &gated.task_id,
            crate::application::task::TaskUpdateParams {
                title: Some("System-updated fixture".to_string()),
                ..Default::default()
            },
            Some(gated.run_id),
        )
        .expect("system edit");
    let history = runtime.get_task_history(&gated.task_id).expect("history");
    assert_eq!(&history[..before.len()], before.as_slice());
    assert_eq!(history.len(), before.len() + 1);
    assert_eq!(history[before.len()].event, "renamed");
    assert_eq!(
        history[before.len()].by,
        "system",
        "ORB-13916: deterministic history must not masquerade as human intervention"
    );
}

#[test]
fn an_over_budget_pass_cannot_issue_a_certificate() {
    let gated = gated_fixture(
        "[crews.reviewers]\nmodel = \"review-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[crews.implementer]\nmodel = \"impl-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[workflow]\ndefault_crew = \"implementer\"\n[operation]\nreview_policy = \"before-pr\"\nreview_crew = \"reviewers\"\nreview_minutes = 1\n",
    );
    let task = gated
        .fixture
        .runtime
        .get_task(&gated.task_id)
        .expect("task");
    let digest = task_meaning_digest(&task).expect("digest");
    let combined =
        combined_task_meaning_digest(&[(task.id.to_string(), digest)]).expect("combined");
    let workspace_id = gated.fixture.runtime.workspace_id().expect("workspace");
    let lineage = lineage_key(&workspace_id, std::slice::from_ref(&gated.task_id), "main");
    let started = Utc::now() - Duration::minutes(2);
    let candidate = SourceRevision {
        commit: gated.implementation_sha.clone(),
        tree: git(&gated.fixture.repo, &["rev-parse", "HEAD^{tree}"]),
    };
    let store = gated.fixture.runtime.review_store().expect("store");
    let ReviewReservation::Reserved { attempt } = store
        .review_reserve(
            &workspace_id,
            &ReviewReserveRequest {
                lineage_key: &lineage,
                task_ids: std::slice::from_ref(&gated.task_id),
                run_id: &gated.run_id,
                task_meaning_digest: &combined,
                candidate: &candidate,
                budget: ReviewBudget {
                    reviewer_starts: 2,
                    repair_cycles: 2,
                    minutes: 1,
                },
                now: started,
            },
        )
        .expect("pre-reserve")
        .0
    else {
        panic!("pre-reserve a start two minutes ago");
    };

    let admission = gated.admit().expect("resume the overdue attempt");
    assert_eq!(admission["decision"], "resumed");
    assert_eq!(admission["attempt_id"], attempt.attempt_id);
    assert_eq!(
        admission["remaining"]["seconds"].as_u64(),
        Some(0),
        "the leftover allowance for this invocation is already spent"
    );
    write_report(
        &gated.fixture.runtime,
        &gated.task_id,
        &report(
            &attempt.attempt_id,
            ReviewVerdict::PassedWithoutRepairs,
            false,
        ),
    );
    let error = gated.settle(&admission).expect_err("over budget");
    let message = error.to_string();
    assert!(message.contains("review_minutes_exhausted"), "{message}");
    let certificate = gated.certificate();
    assert_eq!(certificate.verdict, ReviewVerdict::Incomplete);
    assert!(!certificate.verdict.passed());
    assert_eq!(certificate.budget.minutes, 1);
    assert!(certificate.consumed.seconds >= 120);
}
