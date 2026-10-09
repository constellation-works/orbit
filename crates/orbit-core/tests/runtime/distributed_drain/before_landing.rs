//! A claimed leaf from an owner with `review.before_landing` on reviews its
//! open pull request on the leaf, after `pr_open` and before `handoff`, and
//! the owner accepts — and so can land — only a handoff whose before-landing
//! review settled the exact head handed off [ORB-14849].

use super::claimed_review::{ReviewedLeaf, revision};
use super::*;

use orbit_types::workflow::handoff::HandoffReviewEvidence;
use orbit_types::workflow::{ReviewAdmission, ReviewVerdict};

/// The claim captures the owner's before-landing review. On the leaf the
/// pre-push gate does not apply and the before-landing gate does; its
/// reviewer's fix is the reviewed head. The owner refuses a handoff without
/// that review, with it filed as a before-PR review, or for a head pushed
/// after it settled, and accepts the reviewed head as before-landing
/// evidence.
#[test]
fn a_claimed_pr_lands_only_at_the_head_its_before_landing_review_settled() {
    if !isolated(
        module_path!(),
        "a_claimed_pr_lands_only_at_the_head_its_before_landing_review_settled",
    ) {
        return;
    }
    let mut leaf = ReviewedLeaf::admit_before_landing();
    let pulls = leaf.pair.wire.calls("orbit.task.pull");
    assert_eq!(pulls[0]["review_gate"], true, "{}", pulls[0]);
    assert_eq!(pulls[0]["ship"]["before_landing"], true, "{}", pulls[0]);
    assert_eq!(pulls[0]["ship"]["before_pr"], false, "{}", pulls[0]);
    assert_eq!(pulls[0]["ship"]["review"]["crew"], "sol");

    let input = leaf
        .pair
        .follower_jobs
        .get_job_run(&leaf.leaf)
        .unwrap()
        .unwrap()
        .input
        .unwrap();
    let admission = ReviewAdmission::from_run_input(&input)
        .unwrap()
        .expect("the leaf carries the claim's review admission");
    assert!(admission.gates_landing() && !admission.gates_pr());

    let pre_push = leaf.admit_review();
    assert_eq!(pre_push["applies"], false, "{pre_push}");
    assert_eq!(pre_push["reason"], "review_before_landing");

    leaf.gate_input["before_landing"] = json!(true);
    let admitted = leaf.admit_review();
    assert_eq!(admitted["applies"], true, "{admitted}");
    let attempt_id = admitted["attempt_id"].as_str().unwrap().to_string();
    leaf.reviewer_reports(&attempt_id, ReviewVerdict::AcceptWithFixes, true);
    let settled = leaf.settle().expect("an accepted review passes");
    assert_eq!(settled["reviewer_fixed"], true, "{settled}");
    let reviewed = revision(&leaf.pair.follower_repo, "HEAD");
    assert_eq!(settled["reviewed_head_sha"], reviewed.commit.as_str());
    let evidence: HandoffReviewEvidence =
        serde_json::from_value(settled["handoff_evidence"].clone()).expect("handoff evidence");

    let refused = leaf
        .owner_accepts_review(HandoffReview::not_required(), &reviewed)
        .expect_err("an unreviewed handoff is refused");
    assert!(
        refused.to_string().contains("review_evidence_missing"),
        "{refused}"
    );
    let refused = leaf
        .owner_accepts(evidence.clone(), &reviewed)
        .expect_err("a before-PR review cannot stand in for the captured timing");
    assert!(
        refused.to_string().contains("review_evidence_missing"),
        "{refused}"
    );

    // A head pushed after the review settled was never reviewed.
    std::fs::write(
        leaf.pair.follower_repo.join("src/f0.rs"),
        "fn work() {}\n// pushed after review\n",
    )
    .unwrap();
    git(&leaf.pair.follower_repo, &["commit", "-q", "-am", "Later"]);
    let later = revision(&leaf.pair.follower_repo, "HEAD");
    let landing_review = |evidence: &HandoffReviewEvidence| HandoffReview {
        policy: ReviewTiming::BeforeLanding,
        disposition: HandoffReviewDisposition::BeforeLanding(Box::new(evidence.clone())),
    };
    let refused = leaf
        .owner_accepts_review(landing_review(&evidence), &later)
        .expect_err("an unreviewed head is refused");
    assert!(
        refused.to_string().contains("reviewed_head_mismatch"),
        "{refused}"
    );
    assert_eq!(leaf.pair.owner_status(&leaf.task), "in-progress");

    leaf.owner_accepts_review(landing_review(&evidence), &reviewed)
        .expect("the owner accepts the before-landing review of the handed-off head");
    assert_eq!(leaf.pair.owner_status(&leaf.task), "review");
}
