//! A before-landing handoff the owner merged after its landing branch moved
//! is still covered when the provider merged the reviewed head and the
//! landing is exactly the reviewed change carried cleanly onto the moved base
//! [ORB-15193]. A conflicting base change or a head pushed after the review
//! stays an after-landing obligation.

use super::before_pr::{ReviewedHandoff, revision};
use super::*;

use orbit_automation::review::REBASED_CLEAN_ASSURANCE;
use orbit_types::workflow::automation::AutomationState;

/// Land an unrelated change on `main` as pull request #41 after the review
/// settled, so the reviewed base is no longer the landing base.
fn advance_main(reviewed: &ReviewedHandoff, file: &str, content: &str) {
    std::fs::write(reviewed.repo.join(file), content).unwrap();
    git(&reviewed.repo, &["add", "-A"]);
    git(&reviewed.repo, &["commit", "-q", "-m", "Unrelated #41"]);
    let head = git(&reviewed.repo, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    reviewed.merged_pull_request(41, &head);
}

fn pending_keys(state: &AutomationState) -> Vec<&str> {
    state
        .pending
        .iter()
        .map(|delivery| delivery.key.as_str())
        .collect()
}

/// The reviewed head squash-lands onto a base an unrelated pull request
/// advanced. The landing is the conflict-free merge of the reviewed change
/// onto that base, so the after-landing consumer excludes it under the
/// distinct `rebased_clean` assurance and queues only the unrelated landing.
#[test]
fn a_before_landing_handoff_squashed_onto_a_moved_base_is_excluded_as_rebased_clean() {
    if !isolated(
        module_path!(),
        "a_before_landing_handoff_squashed_onto_a_moved_base_is_excluded_as_rebased_clean",
    ) {
        return;
    }
    let reviewed = ReviewedHandoff::accept(ReviewTiming::BeforeLanding);
    advance_main(&reviewed, "src/other.rs", "fn other() {}\n");
    let landed = reviewed.squash_merge(42, &reviewed.candidate.commit);
    assert_ne!(
        revision(&reviewed.repo, &landed).tree,
        reviewed.candidate.tree,
        "the landing is not the reviewed tree byte for byte"
    );

    let state = reviewed.observe();
    assert_eq!(
        pending_keys(&state),
        vec!["pr:owner/repository:main:41"],
        "{state:#?}"
    );
    let [excluded] = state.excluded.as_slice() else {
        panic!("one exclusion expected: {state:#?}");
    };
    assert_eq!(excluded.delivery.key, "pr:owner/repository:main:42");
    assert_eq!(excluded.delivery.after.commit, landed);
    assert_eq!(excluded.exclusion.assurance, REBASED_CLEAN_ASSURANCE);
    assert_eq!(
        excluded.exclusion.attempt_id,
        reviewed.certificate.attempt_id
    );
    assert_eq!(
        excluded.exclusion.final_candidate_tree,
        reviewed.candidate.tree
    );
}

/// Neither a base change the reviewed patch conflicts with, resolved by hand
/// at landing, nor a head pushed after the review settled is covered by the
/// rebased-clean rule: both landings stay pending for after-landing review.
#[test]
fn a_conflicting_base_or_a_head_pushed_after_review_stays_pending() {
    if !isolated(
        module_path!(),
        "a_conflicting_base_or_a_head_pushed_after_review_stays_pending",
    ) {
        return;
    }

    // The base rewrote the line the reviewed patch extends; the landing is a
    // hand resolution the reviewer never saw, under the reviewed head.
    let reviewed = ReviewedHandoff::accept(ReviewTiming::BeforeLanding);
    advance_main(&reviewed, "src/work.rs", "fn work() { base() }\n");
    std::fs::write(
        reviewed.repo.join("src/work.rs"),
        "fn work() { base() }\n// reviewed\n",
    )
    .unwrap();
    git(&reviewed.repo, &["commit", "-q", "-am", "Squash-merge #42"]);
    reviewed.merged_pull_request(42, &reviewed.candidate.commit);
    let state = reviewed.observe();
    assert!(state.excluded.is_empty(), "conflict: {state:#?}");
    assert_eq!(
        pending_keys(&state),
        vec!["pr:owner/repository:main:41", "pr:owner/repository:main:42"],
        "conflict: {state:#?}"
    );

    // A commit pushed after the review leaves the reviewed tree unchanged,
    // so the landing equals the clean merge; the provider merged a head that
    // was never reviewed, and that alone keeps it pending.
    let reviewed = ReviewedHandoff::accept(ReviewTiming::BeforeLanding);
    advance_main(&reviewed, "src/other.rs", "fn other() {}\n");
    git(&reviewed.repo, &["checkout", "-q", &reviewed.branch]);
    git(
        &reviewed.repo,
        &["commit", "-q", "--allow-empty", "-m", "Pushed after review"],
    );
    let later = git(&reviewed.repo, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    git(&reviewed.repo, &["checkout", "-q", "main"]);
    reviewed.squash_merge(42, &later);
    let state = reviewed.observe();
    assert!(state.excluded.is_empty(), "later head: {state:#?}");
    assert_eq!(
        pending_keys(&state),
        vec!["pr:owner/repository:main:41", "pr:owner/repository:main:42"],
        "later head: {state:#?}"
    );
}
