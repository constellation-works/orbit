//! Submission captures review policy before configuration changes.

use super::support::{BEFORE_PR, gated_fixture};

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
