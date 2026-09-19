//! Interrupted commits: what a failure before the decision leaves, what a
//! failure after it owes, and what happens when recovery itself fails.

use tempfile::TempDir;

use super::*;

/// Reopen the same root with fresh handles — what a restarted process sees.
fn reopen(temp: &TempDir) -> Coordinated {
    Coordinated::open(temp.path())
}

#[test]
fn a_failure_before_the_decision_publishes_neither_half() {
    let temp = TempDir::new().expect("tempdir");
    let coordinated = Coordinated::open(temp.path());
    let task = coordinated.create_task("Interrupted");
    let before = coordinated.history(&task.id).len();

    inject_coordination_faults(&[CoordinationFault::BeforeCommit]);
    let error = coordinated
        .boundary()
        .commit_task_transition(&coordinated.admission_params(&task.id, "src/lib.rs"))
        .expect_err("the injected failure must surface");
    assert!(error.to_string().contains("BeforeCommit"), "{error}");

    assert_eq!(coordinated.task(&task.id).status, TaskStatus::Backlog);
    assert_eq!(coordinated.history(&task.id).len(), before);
    assert!(coordinated.active_reservations().is_empty());
    assert!(
        coordinated
            .boundary()
            .store_handle()
            .unsettled_task_commit_journal(PARTITION_ID)
            .expect("journal")
            .is_empty(),
        "compensation abandons the undecided row"
    );
    assert!(!coordinated.boundary().pending_marker_exists());

    // And the partition is still usable: the next commit succeeds normally.
    committed(
        coordinated
            .boundary()
            .commit_task_transition(&coordinated.admission_params(&task.id, "src/lib.rs"))
            .expect("retry"),
    );
    assert_eq!(coordinated.task(&task.id).status, TaskStatus::InProgress);
}

#[test]
fn a_failure_after_the_decision_recovers_both_halves_on_reopen() {
    let temp = TempDir::new().expect("tempdir");
    let task_id = {
        let coordinated = Coordinated::open(temp.path());
        let task = coordinated.create_task("Decided");

        inject_coordination_faults(&[CoordinationFault::AfterCommit]);
        let error = coordinated
            .boundary()
            .commit_task_transition(&coordinated.admission_params(&task.id, "src/lib.rs"))
            .expect_err("the injected failure must surface");
        assert!(error.to_string().contains("AfterCommit"), "{error}");

        // The decision is durable; the bundle has not caught up yet.
        assert_eq!(coordinated.active_reservations().len(), 1);
        assert!(
            coordinated.boundary().pending_marker_exists(),
            "an unapplied decision must stay visible to the next entrant"
        );
        task.id
    };

    // A fresh composition is what a restarted process gets. The first read
    // settles the decision before it exposes any state.
    let restarted = reopen(&temp);
    assert_eq!(restarted.task(&task_id).status, TaskStatus::InProgress);
    assert!(
        restarted
            .history(&task_id)
            .iter()
            .any(|entry| entry.event == "pulled_by"),
        "the history half of the decision lands too"
    );
    assert_eq!(restarted.active_reservations().len(), 1);
    assert!(!restarted.boundary().pending_marker_exists());
    assert!(
        restarted
            .boundary()
            .store_handle()
            .unsettled_task_commit_journal(PARTITION_ID)
            .expect("journal")
            .is_empty(),
        "recovery settles the journal row it rolled forward"
    );
}

#[test]
fn an_interrupted_apply_replays_deterministically() {
    let temp = TempDir::new().expect("tempdir");
    let coordinated = Coordinated::open(temp.path());
    let task = coordinated.create_task("Half applied");
    let before = coordinated.history(&task.id).len();

    inject_coordination_faults(&[CoordinationFault::DuringApply]);
    coordinated
        .boundary()
        .commit_task_transition(&coordinated.admission_params(&task.id, "src/lib.rs"))
        .expect_err("the injected failure must surface");

    // A crash mid-append leaves a partial row. The intent records the
    // pre-apply length, so the replay cuts the tail rather than parsing it.
    let events_path = coordinated
        .boundary()
        .bundle_store
        .bundle_path(&task.id)
        .expect("bundle path")
        .join("events.jsonl");
    let torn = format!(
        "{}{{\"schema_version\":1,\"event_id\":\"EV-00",
        std::fs::read_to_string(&events_path).expect("read events")
    );
    std::fs::write(&events_path, torn).expect("simulate a torn append");

    // Recovery on the same handles, then again on fresh ones: the replay is
    // idempotent, so the event lands exactly once either way.
    coordinated.boundary().recover().expect("recover");
    reopen(&temp).boundary().recover().expect("recover again");

    let history = coordinated.history(&task.id);
    assert_eq!(history.len(), before + 1);
    assert_eq!(
        history
            .iter()
            .filter(|entry| entry.event == "pulled_by")
            .count(),
        1,
        "a replayed apply must not duplicate its history"
    );
    assert_eq!(coordinated.task(&task.id).status, TaskStatus::InProgress);
    assert_eq!(coordinated.active_reservations().len(), 1);
}

#[test]
fn a_failed_compensation_keeps_the_partition_closed_until_recovery_succeeds() {
    let temp = TempDir::new().expect("tempdir");
    let coordinated = Coordinated::open(temp.path());
    let task = coordinated.create_task("Fail closed");

    inject_coordination_faults(&[
        CoordinationFault::BeforeCommit,
        CoordinationFault::DuringCompensation,
    ]);
    let error = coordinated
        .boundary()
        .commit_task_transition(&coordinated.admission_params(&task.id, "src/lib.rs"))
        .expect_err("the injected compensation failure must surface");
    assert!(error.to_string().contains("DuringCompensation"), "{error}");
    assert!(
        coordinated.boundary().pending_marker_exists(),
        "a failed compensation leaves the marker for the next entrant"
    );

    // Recovery fails too: every entry point refuses rather than exposing a
    // partition whose commit state is unknown.
    inject_coordination_faults(&[CoordinationFault::DuringRecovery]);
    let read_error = coordinated
        .backends
        .task
        .task
        .get_task(&task.id)
        .expect_err("a read must not expose state before recovery settles");
    assert!(
        read_error.to_string().contains("DuringRecovery"),
        "{read_error}"
    );

    inject_coordination_faults(&[CoordinationFault::DuringRecovery]);
    let write_error = coordinated
        .backends
        .reservation
        .reserve_task_reservation(coordinated.reservation_params(&task.id, "docs/x.md"))
        .expect_err("a reservation mutation must fail closed too");
    assert!(
        write_error.to_string().contains("DuringRecovery"),
        "{write_error}"
    );

    inject_coordination_faults(&[CoordinationFault::DuringRecovery]);
    let show_error = coordinated
        .backends
        .reservation
        .show_workspace_claim(&coordinated.orbit_dir.to_string_lossy(), Some(PARTITION_ID))
        .expect_err("a mutating claim show must fail closed too");
    assert!(
        show_error.to_string().contains("DuringRecovery"),
        "{show_error}"
    );

    // With no injected failure the next entry settles the undecided row and
    // the partition is open again, at its pre-commit state.
    assert_eq!(coordinated.task(&task.id).status, TaskStatus::Backlog);
    assert!(coordinated.active_reservations().is_empty());
    assert!(!coordinated.boundary().pending_marker_exists());
    assert!(
        coordinated
            .boundary()
            .store_handle()
            .unsettled_task_commit_journal(PARTITION_ID)
            .expect("journal")
            .is_empty()
    );
}

#[test]
fn an_entrant_recovers_a_commit_that_failed_after_its_initial_check() {
    // Hold a partition lock only, to simulate a commit which was already
    // publishing before this entrant checked its pending marker.
    let temp = TempDir::new().expect("temp");
    let fixture = Coordinated::open(temp.path());
    let task = fixture.create_task("gap");
    let entrant = Coordinated::open(temp.path());
    let (checked_tx, checked_rx) = std::sync::mpsc::sync_channel(1);
    let (continue_tx, continue_rx) = std::sync::mpsc::sync_channel(1);
    std::thread::scope(|scope| {
        let writer = scope.spawn(|| {
            BEFORE_ORDINARY_LOCK.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    checked_tx.send(()).expect("signal check");
                    continue_rx
                        .recv_timeout(std::time::Duration::from_secs(10))
                        .expect("continue");
                }))
            });
            entrant
                .boundary()
                .enter_ordinary(|| {
                    let bundle = entrant
                        .boundary()
                        .bundle_store
                        .read_bundle_lightweight(&task.id)?;
                    Ok(bundle.envelope.status)
                })
                .expect("recovered read")
        });
        checked_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("initial check");
        // Bypass the outer host lock only in this fault-injection fixture:
        // reproduce the pending marker arriving after the cheap check.
        inject_coordination_faults(&[CoordinationFault::AfterCommit]);
        let _depth = BoundaryDepth::enter(&fixture.boundary().partition_dir);
        let error = fixture
            .boundary()
            .commit_locked(&fixture.admission_params(&task.id, "src/lib.rs"))
            .expect_err("lost apply");
        assert!(error.to_string().contains("AfterCommit"));
        drop(_depth);
        continue_tx.send(()).expect("release entrant");
        assert_eq!(writer.join().expect("thread"), TaskStatus::InProgress);
    });
}
