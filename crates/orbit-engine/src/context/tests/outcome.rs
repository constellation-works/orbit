//! Regression guard for the `workflow_run_failed` history note.
//!
//! Measurement over 845 task bundles (ORB-10343) found that every history entry
//! over 2 KB was a `workflow_run_failed` note inlining a run's whole
//! `error_message` — 9 entries carrying 16.6% of all history bytes. These tests
//! pin the bound so that surface cannot silently return to inlining bulk.

use orbit_types::task::TaskStatus;

use crate::context::outcome::{
    MAX_NOTE_ERROR_BYTES, WORKFLOW_RUN_FAILED_EVENT, WORKFLOW_RUN_INTERRUPTED_EVENT,
    blocked_workflow_failure_update, blocked_workflow_interruption_update, workflow_failure_note,
    workflow_interruption_note,
};

const JOB_ID: &str = "task_pr_pipeline";
const RUN_ID: &str = "jrun-20260720-0146-3";

/// The overwhelming majority of real failures (p95 = 676 B) must read exactly
/// as they did before the cap — an elision that fires on ordinary errors would
/// cost more diagnostic value than the bulk it saves.
#[test]
fn short_error_message_is_inlined_verbatim() {
    let error = "worktree integrity violation `worktree_integrity_ambiguous`";
    let note = workflow_failure_note(JOB_ID, RUN_ID, Some("E_INTEGRITY"), Some(error));

    assert_eq!(
        note,
        format!(
            "workflow run failed: job={JOB_ID}, run_id={RUN_ID}, \
             error_code=E_INTEGRITY, error={error}"
        )
    );
    assert!(!note.contains("elided"));
}

/// A message exactly at the threshold is still inlined; the cap is inclusive so
/// there is no off-by-one band that elides without need.
#[test]
fn error_message_at_threshold_is_inlined_verbatim() {
    let error = "x".repeat(MAX_NOTE_ERROR_BYTES);
    let note = workflow_failure_note(JOB_ID, RUN_ID, None, Some(&error));

    assert!(note.ends_with(&error));
    assert!(!note.contains("elided"));
}

/// The ORB-10332 shape: a worktree-integrity failure serializing its whole
/// `dirty_paths` list. The note must stay small and must say where the rest is.
#[test]
fn oversized_error_message_is_elided_with_retrieval_path() {
    let error = format!(
        "execution failed: v2 job dispatch: worktree integrity violation: {}",
        "\"crates/orbit-cli/src/command/task/command.rs\",".repeat(2_000)
    );
    assert!(
        error.len() > 80_000,
        "fixture must reproduce the real shape"
    );

    let note = workflow_failure_note(JOB_ID, RUN_ID, Some("-"), Some(&error));

    // Bounded: threshold + a fixed pointer suffix + the short envelope. The
    // real note this replaces was 85,005 B.
    assert!(
        note.len() < 2 * MAX_NOTE_ERROR_BYTES,
        "note must stay bounded, got {} B",
        note.len()
    );
    // The head is preserved, so the note is still self-explanatory at a glance.
    assert!(note.contains("worktree integrity violation"));
    // The retrieval path travels with the elision, not in separate docs.
    assert!(note.contains(&format!("orbit run show {RUN_ID} --json")));
    assert!(note.contains(".run.steps[].error_message"));
    assert!(note.contains(&format!("error_message is {} B", error.len())));
}

/// Error text is arbitrary bytes from a failing subprocess. Slicing it
/// mid-codepoint would panic inside the terminalization path that is meant to
/// be best-effort.
#[test]
fn elision_boundary_is_utf8_safe() {
    // A 4-byte character straddles the cap for every offset in 0..4.
    for pad in 0..4 {
        let error = format!(
            "{}{}",
            "a".repeat(MAX_NOTE_ERROR_BYTES - 1 + pad),
            "🛰".repeat(200)
        );
        let note = workflow_failure_note(JOB_ID, RUN_ID, None, Some(&error));
        assert!(note.contains("elided"), "pad={pad} must elide");
        assert!(note.len() < 2 * MAX_NOTE_ERROR_BYTES, "pad={pad}");
    }
}

/// The automation update that every failure path uses must go through the
/// capped helper. A writer that assembled its own note would reintroduce the
/// blob without any test noticing.
#[test]
fn blocked_update_routes_through_the_capped_note() {
    let error = "y".repeat(50_000);
    let update = blocked_workflow_failure_update(JOB_ID, RUN_ID, None, Some(&error));

    assert_eq!(
        update.status_event.as_deref(),
        Some(WORKFLOW_RUN_FAILED_EVENT)
    );
    let note = update.status_note.expect("blocked update carries a note");
    assert_eq!(
        note,
        workflow_failure_note(JOB_ID, RUN_ID, None, Some(&error))
    );
    assert!(note.len() < 2 * MAX_NOTE_ERROR_BYTES);
}

/// An interrupted run's block must read as an interruption, not a failure, so
/// an operator can tell "resume it" from "diagnose it" — while keeping the
/// `run_id=…,` shape that failure-note readers already match on.
#[test]
fn interruption_update_names_the_interruption_and_its_resume() {
    let update = blocked_workflow_interruption_update(
        JOB_ID,
        RUN_ID,
        Some("process_not_found"),
        Some("worker pid 4242 is gone"),
    );

    assert_eq!(update.status, Some(TaskStatus::Blocked));
    assert_eq!(
        update.status_event.as_deref(),
        Some(WORKFLOW_RUN_INTERRUPTED_EVENT)
    );
    assert_ne!(WORKFLOW_RUN_INTERRUPTED_EVENT, WORKFLOW_RUN_FAILED_EVENT);
    let note = update.status_note.expect("blocked update carries a note");
    assert_eq!(
        note,
        workflow_interruption_note(
            JOB_ID,
            RUN_ID,
            Some("process_not_found"),
            Some("worker pid 4242 is gone"),
        )
    );
    assert!(note.starts_with("workflow run interrupted:"), "{note}");
    assert!(note.contains(&format!("run_id={RUN_ID},")), "{note}");
    assert!(note.contains("error_code=process_not_found"), "{note}");
    assert!(
        note.contains(&format!("orbit job resume {RUN_ID}")),
        "{note}"
    );
}

/// The interruption note shares the failure note's elision cap.
#[test]
fn interruption_note_is_capped() {
    let error = "z".repeat(50_000);
    let note = workflow_interruption_note(JOB_ID, RUN_ID, None, Some(&error));
    assert!(note.contains("elided"));
    assert!(note.len() < 2 * MAX_NOTE_ERROR_BYTES);
}
