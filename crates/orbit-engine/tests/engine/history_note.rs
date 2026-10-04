#![allow(missing_docs)]
// Integration fixtures exercise public behavior and unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Runtime bound on the `workflow_run_failed` history note [ORB-10343].
//!
//! Measurement over 845 task bundles found that every history entry over 2 KB
//! was a `workflow_run_failed` note inlining a run's whole `error_message` — 9
//! entries carrying 16.6% of all history bytes, one of them 85 KB.
//! `scripts/check-history-note-size.sh` pins the structural side (one
//! threshold, one producer); these tests pin what the blocked-task updates
//! actually write.

use orbit_engine::{
    WORKFLOW_RUN_FAILED_EVENT, WORKFLOW_RUN_INTERRUPTED_EVENT, blocked_workflow_failure_update,
    blocked_workflow_interruption_update,
};
use orbit_types::task::TaskStatus;

const JOB_ID: &str = "task_pr_pipeline";
const RUN_ID: &str = "jrun-20260720-0146-3";
/// The size the incident's oversized entries all exceeded.
const BULK_BYTES: usize = 2_000;

fn failure_note(error: &str) -> String {
    let update = blocked_workflow_failure_update(JOB_ID, RUN_ID, None, Some(error));
    assert_eq!(update.status, Some(TaskStatus::Blocked));
    assert_eq!(
        update.status_event.as_deref(),
        Some(WORKFLOW_RUN_FAILED_EVENT)
    );
    update.status_note.expect("blocked update carries a note")
}

/// An ordinary failure (the measured p95 is 676 B) reads verbatim. The
/// ORB-10332 shape — a worktree-integrity failure serializing its whole
/// `dirty_paths` list — stays bounded and says where the full text lives.
/// Error text is arbitrary subprocess bytes, so the cut must never land
/// mid-codepoint and panic the best-effort terminalization path.
#[test]
fn a_failure_note_inlines_ordinary_errors_and_elides_bulk_with_its_retrieval_path() {
    let ordinary = format!("worktree integrity violation: {}", "p".repeat(640));
    let note = failure_note(&ordinary);
    assert_eq!(
        note,
        format!(
            "workflow run failed: job={JOB_ID}, run_id={RUN_ID}, error_code=-, error={ordinary}"
        )
    );

    let bulk = format!(
        "execution failed: v2 job dispatch: worktree integrity violation: {}",
        "\"crates/orbit-cli/src/command/task/command.rs\",".repeat(2_000)
    );
    assert!(bulk.len() > 80_000, "fixture must reproduce the real shape");
    let note = failure_note(&bulk);
    assert!(
        note.len() < BULK_BYTES,
        "note must stay bounded, got {} B",
        note.len()
    );
    assert!(note.contains("worktree integrity violation"), "{note}");
    assert!(
        note.contains(&format!("orbit run show {RUN_ID} --json")),
        "{note}"
    );
    assert!(note.contains(".run.steps[].error_message"), "{note}");
    assert!(
        note.contains(&format!("error_message is {} B", bulk.len())),
        "{note}"
    );

    // A run of 4-byte characters straddles any cut for one of these offsets.
    for pad in 0..4 {
        let error = format!("{}{}", "a".repeat(pad), "🛰".repeat(1_000));
        let note = failure_note(&error);
        assert!(note.contains("elided"), "pad={pad}");
        assert!(note.len() < BULK_BYTES, "pad={pad}");
    }
}

/// An interrupted run's block reads as an interruption with its resume
/// command, keeps the `run_id=…,` shape readers match on, and shares the cap.
#[test]
fn an_interruption_note_names_its_resume_and_shares_the_cap() {
    let error = "z".repeat(50_000);
    let update = blocked_workflow_interruption_update(
        JOB_ID,
        RUN_ID,
        Some("process_not_found"),
        Some(&error),
    );

    assert_eq!(update.status, Some(TaskStatus::Blocked));
    assert_eq!(
        update.status_event.as_deref(),
        Some(WORKFLOW_RUN_INTERRUPTED_EVENT)
    );
    let note = update.status_note.expect("blocked update carries a note");
    assert!(note.starts_with("workflow run interrupted:"), "{note}");
    assert!(note.contains(&format!("run_id={RUN_ID},")), "{note}");
    assert!(note.contains("error_code=process_not_found"), "{note}");
    assert!(
        note.contains(&format!("orbit job resume {RUN_ID}")),
        "{note}"
    );
    assert!(note.contains("elided"), "{note}");
    assert!(
        note.len() < BULK_BYTES,
        "note must stay bounded, got {} B",
        note.len()
    );
}
