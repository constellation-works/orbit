use orbit_types::task::history_note_summary;
use serde_json::Value;
use serde_json::json;

const SHA: &str = "a5ca9ee70220135af6dfd892ec470b998c0e9385";

fn assert_human(note: &Value, expected: &str) {
    let summary = history_note_summary(&note.to_string());
    assert_eq!(summary, expected);
    assert!(summary.chars().count() <= 76);
    for banned in ["schema_version", "operation_id", "run_id", "task_id", SHA] {
        assert!(!summary.contains(banned), "{summary} contains {banned}");
    }
}

#[test]
fn host_operational_held_shows_disposition() {
    assert_human(
        &json!({
            "operation_id": "96f67fdeb9907a2889b41784b7473d706df0892d0000000000000000000000ab",
            "disposition": "host_operational",
            "schema_version": 1,
        }),
        "host_operational",
    );
}

#[test]
fn decision_requested_shows_summary_not_ids() {
    assert_human(
        &json!({
            "id": "dr-0aa753f1f8a891eb",
            "task_id": "ORB-28985",
            "summary": "Choose how to publish the repair",
            "schema_version": 1,
        }),
        "Choose how to publish the repair",
    );
}

#[test]
fn source_repair_observation_has_no_bookkeeping() {
    assert_human(
        &json!({
            "schema_version": 1,
            "run_id": "jrun-20261126-1048-t1",
            "outcome": "published",
            "head_sha": SHA,
        }),
        "published",
    );
}

#[test]
fn pr_auto_fix_admitted_drops_the_head_sha() {
    assert_human(
        &json!({"pr_number": 10985, "head_sha": SHA, "blocker": "failed_checks"}),
        "failed_checks",
    );
}

#[test]
fn finding_verified_shows_verdict() {
    assert_human(
        &json!({
            "schema_version": 1,
            "verdict": "confirmed",
            "verified_sha": SHA,
            "run_id": "jrun-1",
            "reasoning": "the defect exists",
        }),
        "confirmed",
    );
}

#[test]
fn digest_words_are_stripped_and_long_summaries_bounded() {
    let note = json!({
        "summary": format!("{} sha256:abc {}", "word ".repeat(30), SHA),
        "code": "E1",
    });
    let summary = history_note_summary(&note.to_string());
    assert!(summary.ends_with('…'));
    assert_eq!(summary.chars().count(), 76);
    assert!(!summary.contains("sha256"));
}

#[test]
fn retry_fields_keep_their_labels() {
    let note = json!({"classification": "transient", "attempts": 3, "code": "ci"});
    assert_eq!(
        history_note_summary(&note.to_string()),
        "ci · classification=transient · attempts=3"
    );
}

#[test]
fn notes_without_human_fields_and_plain_text() {
    assert_eq!(
        history_note_summary(r#"{"schema_version":1,"run_id":"x"}"#),
        "structured note"
    );
    assert_eq!(history_note_summary("plain text"), "plain text");
}
