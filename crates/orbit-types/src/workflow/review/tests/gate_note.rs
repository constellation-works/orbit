//! [ORB-15834] Review-gate history notes parse to a run and a tree.

use crate::workflow::ReviewGateHistoryNote;

/// Legacy comma, semicolon and `candidate_tree`-only notes parse to the run
/// and the tree a timeout counts, and each producer renders its historical
/// bytes. Combinatorial pure logic: those rows are no longer written, so a
/// delivery test cannot supply them.
#[test]
fn legacy_shapes_parse_to_run_and_tree_and_render_historical_bytes() {
    let continuation = "Reviewer timed out and settled incomplete. The partial report is retained; delivery is requeued";
    let preservation = ReviewGateHistoryNote::preservation_text(
        "jrun-1",
        "review",
        "aaa",
        "tree-c",
        "tree-i",
        "orbit/candidate",
        continuation,
    );
    let candidate_tree_only = "before-PR review gate stopped delivery: run=jrun-1, failed_step=review, candidate=aaa, candidate_tree=tree-c, branch=orbit/candidate; no PR was opened; held";
    let awaiting = ReviewGateHistoryNote::awaiting_evidence_text("jrun-1", "aaa", None);
    let awaiting_reason =
        ReviewGateHistoryNote::awaiting_evidence_text("jrun-1", "aaa", Some("host released"));
    let received =
        ReviewGateHistoryNote::evidence_received_text("jrun-1", "queued for fresh review");
    let received_held = ReviewGateHistoryNote::evidence_received_text(
        "jrun-1",
        "queued to settle the held review and resume delivery",
    );
    let empty_implementation = "before-PR review gate stopped delivery: run=jrun-1, failed_step=review, candidate=aaa, candidate_tree=tree-c, implementation_tree=, branch=orbit/candidate; no PR was opened; held";

    assert_eq!(
        preservation,
        "before-PR review gate stopped delivery: run=jrun-1, failed_step=review, candidate=aaa, candidate_tree=tree-c, implementation_tree=tree-i, branch=orbit/candidate; no PR was opened; Reviewer timed out and settled incomplete. The partial report is retained; delivery is requeued"
    );
    assert_eq!(
        awaiting,
        "run=jrun-1; candidate=aaa; awaiting named external checks; receipt queues a fresh review"
    );
    assert_eq!(
        awaiting_reason,
        "run=jrun-1; candidate=aaa; awaiting named external checks; receipt queues a fresh review. host released"
    );
    assert_eq!(
        received,
        "run=jrun-1; all named external checks arrived for the held candidate; queued for fresh review."
    );
    assert_eq!(
        received_held,
        "run=jrun-1; all named external checks arrived for the held candidate; queued to settle the held review and resume delivery."
    );

    let cases = [
        (
            preservation.as_str(),
            "jrun-1",
            Some("aaa"),
            Some("tree-c"),
            Some("tree-i"),
            Some("tree-i"),
        ),
        (
            candidate_tree_only,
            "jrun-1",
            Some("aaa"),
            Some("tree-c"),
            None,
            Some("tree-c"),
        ),
        (awaiting.as_str(), "jrun-1", Some("aaa"), None, None, None),
        (
            awaiting_reason.as_str(),
            "jrun-1",
            Some("aaa"),
            None,
            None,
            None,
        ),
        (received.as_str(), "jrun-1", None, None, None, None),
        (received_held.as_str(), "jrun-1", None, None, None, None),
        (
            empty_implementation,
            "jrun-1",
            Some("aaa"),
            Some("tree-c"),
            Some(""),
            Some(""),
        ),
    ];
    for (note, run_id, candidate, candidate_tree, implementation_tree, recorded) in cases {
        let parsed =
            ReviewGateHistoryNote::from_text(note).expect("a review-gate note names a run");
        assert_eq!(parsed.run_id, run_id);
        assert_eq!(parsed.candidate.as_deref(), candidate);
        assert_eq!(parsed.candidate_tree.as_deref(), candidate_tree);
        assert_eq!(parsed.implementation_tree.as_deref(), implementation_tree);
        assert_eq!(parsed.recorded_tree(), recorded);
    }

    assert_eq!(
        ReviewGateHistoryNote::from_text("delivery stopped without a run"),
        None,
        "a note that names no run is not a review-gate note"
    );
}
