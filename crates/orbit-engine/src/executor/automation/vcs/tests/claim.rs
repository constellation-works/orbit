//! Pure helpers of the claimed-leaf handoff steps. Execution-level behavior is
//! covered by the owner-local claimed fixture in orbit-core.
use super::super::claim::{MAX_CAPTURED_OUTPUT_BYTES, capture, pull_request_number, slug};

#[test]
fn remote_urls_reduce_to_owner_and_name_and_a_bare_name_has_none() {
    assert_eq!(
        slug("git@github.com:owner/repo.git").as_deref(),
        Some("owner/repo")
    );
    assert_eq!(
        slug("https://github.com/owner/repo").as_deref(),
        Some("owner/repo")
    );
    assert_eq!(slug("/srv/git/bare.git/").as_deref(), Some("git/bare"));
    assert_eq!(slug("repo"), None);
}

#[test]
fn truncated_capture_reports_that_it_was_truncated() {
    let long = "x".repeat(MAX_CAPTURED_OUTPUT_BYTES + 10);
    let captured = capture(&long, "");
    assert!(captured.starts_with("[truncated to"));
    assert!(captured.len() < long.len() + 64);
}

#[test]
fn capture_joins_both_streams_without_padding_an_empty_one() {
    assert_eq!(capture("out\n", "err\n"), "out\nerr");
    assert_eq!(capture("out\n", "  "), "out");
    assert_eq!(capture("", ""), "");
}

/// [ORB-12617] `pr_open` publishes its number as a string and an exact
/// step-output template forwards that type unchanged, so a pr-mode claim
/// receives `"4242"`, not `4242`. Both shapes name the same pull request.
#[test]
fn a_pull_request_number_is_read_from_either_shape_the_run_can_produce() {
    use serde_json::json;
    assert_eq!(pull_request_number(&json!(4242)), Some(4242));
    assert_eq!(pull_request_number(&json!("4242")), Some(4242));
    assert_eq!(pull_request_number(&json!(" 4242 ")), Some(4242));
    assert_eq!(pull_request_number(&json!("")), None);
    assert_eq!(pull_request_number(&json!("pr-4242")), None);
    assert_eq!(pull_request_number(&json!(null)), None);
}
