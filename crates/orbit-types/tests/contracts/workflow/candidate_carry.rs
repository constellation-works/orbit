//! Persisted failure-hook records remain readable without a state migration.

use orbit_types::workflow::{CandidateCarry, CandidateCarryOutput, FailureActivityCheckpoint};

#[test]
fn candidate_carry_preserves_checkpoint_bytes_and_legacy_defaults() {
    // These are the original producer's layouts, including insertion order
    // (serde_json's preserve_order feature is enabled in this workspace).
    let records = [
        (
            CandidateCarry::None,
            r#"{"phase":"candidate_carry","carry":"none","reason":"no candidate"}"#,
        ),
        (
            CandidateCarry::Failed,
            r#"{"phase":"candidate_carry","carry":"failed","task_id":"task","reason":"protected path"}"#,
        ),
        (
            CandidateCarry::Durable,
            r#"{"phase":"candidate_carry","task_id":"task","branch":"branch","head_sha":"head","carry":"durable","durable_ref":"refs/orbit/candidates/task/run"}"#,
        ),
        (
            CandidateCarry::Failed,
            r#"{"phase":"candidate_carry","task_id":"task","branch":"branch","head_sha":"head","carry":"failed","reason":"push refused"}"#,
        ),
        (
            CandidateCarry::Durable,
            r#"{"phase":"candidate_carry","task_id":"task","branch":"branch","head_sha":"head","partial":true,"carry":"durable","durable_ref":"refs/orbit/candidates/task/run"}"#,
        ),
        (
            CandidateCarry::Failed,
            r#"{"phase":"candidate_carry","task_id":"task","branch":"branch","head_sha":"head","partial":true,"carry":"failed","reason":"push refused"}"#,
        ),
        (
            CandidateCarry::Published,
            r#"{"phase":"candidate_carry","carry":"published","task_id":"task","branch":"branch","head_sha":"head"}"#,
        ),
        (
            CandidateCarry::Durable,
            r#"{"phase":"candidate_carry","carry":"durable","task_id":"task","branch":"branch","head_sha":"head","published_sha":"published","durable_ref":"refs/orbit/candidates/task/run"}"#,
        ),
        (
            CandidateCarry::Failed,
            r#"{"phase":"candidate_carry","carry":"failed","task_id":"task","branch":"branch","head_sha":"head","published_sha":"published","reason":"push refused"}"#,
        ),
    ];
    for (expected, record) in records {
        let mut checkpoint = FailureActivityCheckpoint {
            activity_name: "claim_candidate_carry".into(),
            failed_step_id: "review".into(),
            output: serde_json::from_str(record).unwrap(),
        };
        let carry = CandidateCarryOutput::from_checkpoint(&checkpoint).unwrap();
        assert_eq!(carry.carry, expected);
        assert_eq!(serde_json::to_string(&carry).unwrap(), record);
        assert_eq!(
            serde_json::to_value(&carry).unwrap().to_string(),
            record,
            "the engine serializes via Value before checkpointing"
        );
        checkpoint.activity_name = "another_failure_activity".into();
        assert!(CandidateCarryOutput::from_checkpoint(&checkpoint).is_none());
    }
    for (record, expected) in [
        (r#"{}"#, CandidateCarry::None),
        (r#"{"carry":"unknown"}"#, CandidateCarry::None),
        (r#"{"carry":"failed"}"#, CandidateCarry::Failed),
    ] {
        let checkpoint = FailureActivityCheckpoint {
            activity_name: "claim_candidate_carry".into(),
            failed_step_id: "review".into(),
            output: serde_json::from_str(record).unwrap(),
        };
        let carry = CandidateCarryOutput::from_checkpoint(&checkpoint).unwrap();
        assert_eq!(carry.carry, expected);
        assert!(
            carry.reason.is_none(),
            "consumers retain their missing-reason default"
        );
        assert!(carry.partial.is_none());
        assert!(carry.branch.is_none());
    }
}
