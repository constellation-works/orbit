//! The inherited-failure hold's history note and identity.

use chrono::Utc;
use orbit_types::task::{TaskDecision, TaskHistoryEntry};
use orbit_types::workflow::{PR_AUTO_FIX_BASE_RED_EVENT, PrAutoFixBaseRed};
use serde_json::json;

fn rich(job_log: &str) -> PrAutoFixBaseRed {
    let job = json!({"name": "Check / Clippy / Test", "pull_requests": [{"url": "x"}],
        "steps": [{"name": job_log}]});
    PrAutoFixBaseRed {
        failed_head: "a".repeat(40),
        reason: None,
        landed_base: Some("4d4b9eb0123456789".into()),
        base_branch: Some("agent-main".into()),
        source: Some("latest_push".into()),
        checks: Some(vec![json!({"failed_job": job, "base_job": job,
            "log_excerpt": job_log.repeat(500)})]),
        path: Some("known_red_base".into()),
    }
}

#[test]
fn note_is_one_short_line_and_payload_changes_are_the_same_hold() {
    let held = rich("first");
    let note = held.history_note();
    assert_eq!(
        note,
        "auto-fix held: 'Check / Clippy / Test' also red on base 4d4b9eb; waits for a green base"
    );
    assert!(note.chars().count() <= 200 && !note.contains('\n'));
    assert!(
        held.same_hold(&rich("second")),
        "evidence bytes are not identity"
    );

    let mut moved = rich("first");
    moved.landed_base = Some("5e5e5e5aaaaaaa".into());
    assert!(!held.same_hold(&moved), "another base is another hold");
    let mut other_head = rich("first");
    other_head.failed_head = "b".repeat(40);
    assert!(!held.same_hold(&other_head));
}

#[test]
fn minimal_hold_note_is_clipped_and_legacy_json_notes_still_decode() {
    let reason = format!("base_red: {}", "x".repeat(500));
    let minimal = PrAutoFixBaseRed::for_failed_head("c".repeat(40), reason);
    let note = minimal.history_note();
    assert!(
        note.chars().count() <= 200 && !note.contains('\n'),
        "{note}"
    );
    assert!(
        !minimal.same_hold(&PrAutoFixBaseRed::for_failed_head(
            "c".repeat(40),
            "other".into()
        )),
        "a hold with no named checks is told apart by its reason"
    );

    let legacy = TaskHistoryEntry::new(Utc::now(), "system", PR_AUTO_FIX_BASE_RED_EVENT)
        .with_note(Some(serde_json::to_string(&minimal).unwrap()));
    assert_eq!(PrAutoFixBaseRed::from_entry(&legacy), Some(minimal.clone()));

    let typed = TaskHistoryEntry::new(Utc::now(), "system", PR_AUTO_FIX_BASE_RED_EVENT)
        .with_note(Some(note))
        .with_decision(Some(TaskDecision::PrAutoFixBaseRed(minimal.clone())));
    assert_eq!(PrAutoFixBaseRed::from_entry(&typed), Some(minimal));
}
