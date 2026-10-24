//! Sibling tests for `tool_exec.rs` (migrated per ORB-00246 / docs/design-patterns/test_layout.md).

/// Security invariant: nested persisted identities must not compare scrubbed
/// values, while scrubbed diagnostic prose remains valid output. Crafted
/// nested replies exercise fields unavailable from the read-only probe.
#[test]
fn corrupted_drain_identities_are_detected_in_nested_replies() {
    use crate::tool_exec::corrupted_drain_identity;
    use serde_json::json;

    for field in [
        "protocol_fingerprint",
        "caller_fingerprint",
        "commit",
        "tree",
        "sha256",
        "reviewed_head_sha",
        "reviewed_base_sha",
        "reviewer_commit",
        "covering_commit",
        "final_candidate_tree",
        "head_sha",
        "task_spec_digest",
    ] {
        let reply = json!({"receipt": [{"candidate": {field: "ab[REDACTED_ENV]cd"}}]});
        assert_eq!(
            corrupted_drain_identity(&reply),
            Some(field),
            "protect {field}"
        );
    }
    let reply = json!({"receipt": {"examined_commits": ["intact", "ab[REDACTED_ENV]cd"]}});
    assert_eq!(corrupted_drain_identity(&reply), Some("examined_commits"));
    assert_eq!(
        corrupted_drain_identity(&json!({"diagnostics": ["[REDACTED_ENV]"],
        "candidate": {"commit": "a".repeat(40), "tree": "b".repeat(40)}})),
        None
    );
}
