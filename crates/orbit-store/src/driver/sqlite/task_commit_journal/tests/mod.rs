//! The journal's state machine, exercised directly on a store database.
//!
//! The protocol that uses it lives in `repository::task::coordination`; these
//! tests pin the half that decides: a commit is one transaction, a refusal
//! leaves nothing behind, and only a `prepared` row can be committed.

use tempfile::TempDir;

use crate::Store;
use crate::contracts::{TaskCommitJournalState, TaskCoordinationRow, TaskReservationReserveParams};
use crate::driver::sqlite::task_commit_journal::JournalCommitOutcome;

const WORKSPACE: &str = "orbit-test-123456";
const ORBIT_DIR: &str = "/tmp/orbit-test/.orbit";

fn store(temp: &TempDir) -> Store {
    Store::open(&temp.path().join("state.sqlite")).expect("open store")
}

fn reservation(files: &[&str]) -> TaskReservationReserveParams {
    TaskReservationReserveParams {
        workspace_orbit_dir: ORBIT_DIR.to_string(),
        workspace_id: Some(WORKSPACE.to_string()),
        task_ids: vec!["ORB-0001".to_string()],
        requested_files: files.iter().map(|file| (*file).to_string()).collect(),
        actor: "codex".to_string(),
        ttl_seconds: 600,
        owner_run_id: None,
        owner_metadata_json: None,
    }
}

fn row(row_id: &str) -> TaskCoordinationRow {
    TaskCoordinationRow {
        kind: "admission-receipt".to_string(),
        row_id: row_id.to_string(),
        payload_json: "{\"idle\":false}".to_string(),
    }
}

fn prepare(store: &Store, journal_id: &str) {
    store
        .prepare_task_commit_journal(journal_id, WORKSPACE, "ORB-0001", "{\"intent\":true}")
        .expect("prepare journal row");
}

#[test]
fn commit_publishes_reservation_rows_and_decision_in_one_transaction() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    prepare(&store, "commit-1");

    let outcome = store
        .commit_task_commit_journal(
            "commit-1",
            Some(&reservation(&["src/lib.rs"])),
            &[row("R-1")],
        )
        .expect("commit");

    let JournalCommitOutcome::Committed(reserved) = outcome else {
        panic!("expected a committed decision, got {outcome:?}");
    };
    assert!(reserved.as_ref().is_some_and(|result| result.reserved));
    assert_eq!(
        store.task_commit_journal_state("commit-1").expect("state"),
        Some(TaskCommitJournalState::Committed)
    );
    assert_eq!(
        store
            .task_coordination_rows(WORKSPACE, "admission-receipt")
            .expect("rows")
            .len(),
        1
    );
    assert_eq!(
        store
            .inspect_active_task_reservations(ORBIT_DIR, Some(WORKSPACE))
            .expect("reservations")
            .len(),
        1
    );

    store
        .finish_task_commit_journal("commit-1")
        .expect("settle applied");
    assert_eq!(
        store.task_commit_journal_state("commit-1").expect("state"),
        Some(TaskCommitJournalState::Applied)
    );
    assert!(
        store
            .unsettled_task_commit_journal(WORKSPACE)
            .expect("unsettled")
            .is_empty()
    );
}

#[test]
fn a_reservation_conflict_rolls_back_the_whole_decision() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    prepare(&store, "commit-1");
    store
        .commit_task_commit_journal("commit-1", Some(&reservation(&["src/lib.rs"])), &[])
        .expect("first commit");
    store
        .finish_task_commit_journal("commit-1")
        .expect("settle first");

    prepare(&store, "commit-2");
    let outcome = store
        .commit_task_commit_journal(
            "commit-2",
            Some(&reservation(&["src/lib.rs"])),
            &[row("R-2")],
        )
        .expect("second commit");

    assert!(matches!(outcome, JournalCommitOutcome::Conflicted(result) if !result.reserved));
    assert_eq!(
        store.task_commit_journal_state("commit-2").expect("state"),
        Some(TaskCommitJournalState::Prepared),
        "a refused decision stays undecided for the caller to compensate"
    );
    assert!(
        store
            .task_coordination_rows(WORKSPACE, "admission-receipt")
            .expect("rows")
            .is_empty(),
        "the refused transaction must not leave its coordination rows behind"
    );
}

#[test]
fn a_duplicate_coordination_row_refuses_before_anything_is_written() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    prepare(&store, "commit-1");
    store
        .commit_task_commit_journal("commit-1", None, &[row("R-1")])
        .expect("first commit");
    store
        .finish_task_commit_journal("commit-1")
        .expect("settle first");

    prepare(&store, "commit-2");
    let outcome = store
        .commit_task_commit_journal(
            "commit-2",
            Some(&reservation(&["docs/x.md"])),
            &[row("R-1")],
        )
        .expect("second commit");

    assert!(matches!(
        outcome,
        JournalCommitOutcome::RowExists { ref row_id, .. } if row_id == "R-1"
    ));
    assert!(
        store
            .inspect_active_task_reservations(ORBIT_DIR, Some(WORKSPACE))
            .expect("reservations")
            .is_empty(),
        "a refused replay must not reserve files"
    );
}

#[test]
fn only_a_prepared_row_can_be_committed_or_settled() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);

    let error = store
        .commit_task_commit_journal("missing", None, &[])
        .expect_err("committing an unknown decision must fail closed");
    assert!(error.to_string().contains("not prepared"), "{error}");

    prepare(&store, "commit-1");
    store
        .abort_task_commit_journal("commit-1")
        .expect("abandon undecided intent");
    assert_eq!(
        store.task_commit_journal_state("commit-1").expect("state"),
        Some(TaskCommitJournalState::Aborted)
    );
    assert!(
        store
            .finish_task_commit_journal("commit-1")
            .is_err_and(|error| error.to_string().contains("no longer")),
        "an aborted decision can never be marked applied"
    );
}

#[test]
fn unsettled_records_are_exactly_what_recovery_must_replay() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    prepare(&store, "commit-undecided");
    prepare(&store, "commit-decided");
    store
        .commit_task_commit_journal("commit-decided", None, &[])
        .expect("commit");
    prepare(&store, "commit-settled");
    store
        .abort_task_commit_journal("commit-settled")
        .expect("abort");

    let unsettled = store
        .unsettled_task_commit_journal(WORKSPACE)
        .expect("unsettled");

    let states: Vec<_> = unsettled
        .iter()
        .map(|record| (record.journal_id.as_str(), record.state))
        .collect();
    assert_eq!(
        states,
        vec![
            ("commit-undecided", TaskCommitJournalState::Prepared),
            ("commit-decided", TaskCommitJournalState::Committed),
        ]
    );
    assert!(
        unsettled
            .iter()
            .all(|record| record.intent_json == "{\"intent\":true}"
                && record.task_id == "ORB-0001"),
        "recovery replays the intent it was given, verbatim"
    );
}
