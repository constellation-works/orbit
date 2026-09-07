//! Review lineage ledgers, certificates, and landings [ORB-11333].

use chrono::{Duration, Utc};
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::{
    LandingTransformation, REVIEW_CONTRACT_VERSION, ReviewAttemptState, ReviewBudget,
    ReviewCertificate, ReviewConsumption, ReviewLanding, ReviewReservation, ReviewVerdict,
    ReviewerIdentity,
};

use crate::contracts::{ReviewReserveRequest, ReviewSettlement, ReviewStoreBackend};
use crate::{Store, compose};

const WORKSPACE: &str = "ws_a";
const LINEAGE: &str = "ws_a/ORB-1/agent-main";

fn revision(label: &str) -> SourceRevision {
    SourceRevision {
        commit: format!("commit-{label}"),
        tree: format!("tree-{label}"),
    }
}

fn store() -> std::sync::Arc<dyn ReviewStoreBackend> {
    compose::review_store(Store::open_in_memory().unwrap()).unwrap()
}

fn reserve(
    store: &dyn ReviewStoreBackend,
    run_id: &str,
    candidate: &str,
    digest: &str,
) -> ReviewReservation {
    let task_ids = vec!["ORB-1".to_string()];
    store
        .review_reserve(
            WORKSPACE,
            &ReviewReserveRequest {
                lineage_key: LINEAGE,
                task_ids: &task_ids,
                run_id,
                task_meaning_digest: digest,
                candidate: &revision(candidate),
                budget: ReviewBudget {
                    reviewer_starts: 2,
                    repair_cycles: 2,
                    minutes: 30,
                },
                now: Utc::now(),
            },
        )
        .unwrap()
        .0
}

fn certificate(attempt_id: &str, verdict: ReviewVerdict) -> ReviewCertificate {
    ReviewCertificate {
        schema_version: REVIEW_CONTRACT_VERSION,
        attempt_id: attempt_id.into(),
        lineage_key: LINEAGE.into(),
        task_ids: vec!["ORB-1".into()],
        task_meaning_digest: "meaning".into(),
        repository: "owner/repo".into(),
        base: revision("base"),
        reviewed_candidate: revision("impl"),
        final_candidate: revision("final"),
        implementation_commits: vec![],
        repair_commits: vec![],
        verdict,
        assurance: verdict.assurance(),
        findings: vec![],
        validation: vec![],
        validation_complete: verdict.passed(),
        reviewer: ReviewerIdentity {
            crew: "reviewers".into(),
            provider: "codex".into(),
            model: "gpt-5".into(),
            reasoning_effort: None,
            implementer_model: None,
            same_model_as_implementer: false,
        },
        consumed: ReviewConsumption::default(),
        budget: ReviewBudget::default(),
        escalation: None,
        issued_at: Utc::now(),
    }
}

#[test]
fn reviewer_starts_are_reserved_resumed_and_exhausted_across_the_lineage() {
    let store = store();

    let ReviewReservation::Reserved { attempt: first } =
        reserve(store.as_ref(), "run-1", "impl", "meaning")
    else {
        panic!("first start reserves");
    };
    assert_eq!(first.index, 1);
    assert_eq!(first.state, ReviewAttemptState::Open);

    // A crash before settlement resumes the same attempt on the same candidate.
    let ReviewReservation::Resumed { attempt: resumed } =
        reserve(store.as_ref(), "run-1-retry", "impl", "meaning")
    else {
        panic!("same candidate resumes");
    };
    assert_eq!(resumed.attempt_id, first.attempt_id);

    // A new candidate settles the interrupted attempt as incomplete and
    // consumes the second start.
    let ReviewReservation::Reserved { attempt: second } =
        reserve(store.as_ref(), "run-2", "impl-2", "meaning")
    else {
        panic!("new candidate reserves a second start");
    };
    assert_eq!(second.index, 2);
    let ledger = store.review_ledger(WORKSPACE, LINEAGE).unwrap().unwrap();
    assert_eq!(
        ledger.attempts[0].state,
        ReviewAttemptState::Settled {
            verdict: ReviewVerdict::Incomplete
        }
    );

    let settled = store
        .review_settle(
            WORKSPACE,
            &ReviewSettlement {
                lineage_key: LINEAGE,
                attempt_id: &second.attempt_id,
                verdict: ReviewVerdict::ChangesRequired,
                repair_cycles: 1,
                elapsed_seconds: 90,
                now: Utc::now(),
            },
        )
        .unwrap();
    assert_eq!(settled.consumed().repair_cycles, 1);
    assert_eq!(settled.consumed_seconds, 90);

    // Replaying the settlement is a no-op.
    let replayed = store
        .review_settle(
            WORKSPACE,
            &ReviewSettlement {
                lineage_key: LINEAGE,
                attempt_id: &second.attempt_id,
                verdict: ReviewVerdict::PassedWithoutRepairs,
                repair_cycles: 5,
                elapsed_seconds: 900,
                now: Utc::now(),
            },
        )
        .unwrap();
    assert_eq!(replayed, settled);

    let ReviewReservation::Exhausted { reason, consumed } =
        reserve(store.as_ref(), "run-3", "impl-3", "meaning")
    else {
        panic!("budget is spent");
    };
    assert_eq!(reason, "review_starts_exhausted");
    assert_eq!(consumed.reviewer_starts, 2);
}

#[test]
fn wall_time_exhaustion_refuses_a_further_start() {
    let store = store();
    let ReviewReservation::Reserved { attempt } =
        reserve(store.as_ref(), "run-1", "impl", "meaning")
    else {
        panic!("reserve");
    };
    store
        .review_settle(
            WORKSPACE,
            &ReviewSettlement {
                lineage_key: LINEAGE,
                attempt_id: &attempt.attempt_id,
                verdict: ReviewVerdict::Incomplete,
                repair_cycles: 0,
                elapsed_seconds: 30 * 60,
                now: Utc::now() + Duration::minutes(30),
            },
        )
        .unwrap();
    let ReviewReservation::Exhausted { reason, .. } =
        reserve(store.as_ref(), "run-2", "impl-2", "meaning")
    else {
        panic!("minutes are spent");
    };
    assert_eq!(reason, "review_minutes_exhausted");
}

#[test]
fn certificates_are_immutable_and_looked_up_by_final_tree() {
    let store = store();
    let passed = certificate("rvw-a", ReviewVerdict::PassedWithRepairs);
    store.review_certificate_record(WORKSPACE, &passed).unwrap();
    store.review_certificate_record(WORKSPACE, &passed).unwrap();

    let mut changed = passed.clone();
    changed.verdict = ReviewVerdict::PassedWithoutRepairs;
    let error = store
        .review_certificate_record(WORKSPACE, &changed)
        .expect_err("a changed certificate under the same attempt is refused");
    assert!(error.to_string().contains("different content"), "{error}");

    let blocked = certificate("rvw-b", ReviewVerdict::ChangesRequired);
    store
        .review_certificate_record(WORKSPACE, &blocked)
        .unwrap();

    let found = store
        .review_certificates_for_tree("owner/repo", "tree-final", 10)
        .unwrap();
    assert_eq!(found.len(), 1, "only passed certificates index the tree");
    assert_eq!(found[0].attempt_id, "rvw-a");
    assert!(
        store
            .review_certificates_for_tree("owner/repo", "tree-other", 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .review_certificate(WORKSPACE, "rvw-b")
            .unwrap()
            .unwrap()
            .verdict,
        ReviewVerdict::ChangesRequired
    );
}

#[test]
fn landing_records_replay_idempotently() {
    let store = store();
    let landing = ReviewLanding {
        attempt_id: "rvw-a".into(),
        repository: "owner/repo".into(),
        branch: "agent-main".into(),
        pr_number: Some("7".into()),
        landed: revision("landed"),
        base_at_landing: revision("base"),
        transformation: LandingTransformation::Squash,
        covered: true,
        reason: None,
        recorded_at: Utc::now(),
    };
    store.review_landing_record(&landing).unwrap();
    store.review_landing_record(&landing).unwrap();
    let mut raced = landing.clone();
    raced.landed = revision("other");
    raced.covered = false;
    raced.reason = Some("external_landing_race".into());
    store.review_landing_record(&raced).unwrap();
    let landings = store.review_landings("rvw-a").unwrap();
    assert_eq!(landings.len(), 2);
    assert!(landings.iter().any(|l| !l.covered));
}
