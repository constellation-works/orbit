//! ORB-16389: a claim commit applies after its admission section has
//! released the partition, under its task's bundle lock and commit lease. A
//! committer that dies between its decision and the end of its apply leaves
//! a committed journal row and a dead lease, or no lease at all once a host
//! crash has lost the lease's name. The partition must converge to the
//! committed state, and nothing may use the half-applied bundle first.
//! Journal retention must not take that committed row away.

use std::time::Duration;

use chrono::{TimeDelta, Utc};
use orbit_common::fs::io::FileLockOptions;

use super::super::apply::apply_fault::{self, Fault};
use super::claim_section::Owner;
use crate::contracts::{
    ExecutionClaimPhase, RetentionWorkspaceScope, StoreRetentionBackend, TaskCommitJournalState,
};
use crate::repository::task::v2_bundle::COMMIT_LEASE_DIR;

/// The partition's commit leases, by name.
fn leases(owner: &Owner) -> Vec<std::path::PathBuf> {
    match std::fs::read_dir(owner.boundary.partition_dir.join(COMMIT_LEASE_DIR)) {
        Ok(entries) => entries
            .map(|entry| entry.expect("lease entry").path())
            .collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => panic!("read lease dir: {error}"),
    }
}

/// The event names a bind appended past `before` history entries.
fn appended(owner: &Owner, task_id: &str, before: usize) -> Vec<String> {
    owner
        .tasks
        .get_task_row(task_id, false)
        .expect("read")
        .expect("task")
        .history
        .into_iter()
        .skip(before)
        .map(|entry| entry.event)
        .collect()
}

fn history_len(owner: &Owner, task_id: &str) -> usize {
    owner
        .tasks
        .get_task_row(task_id, false)
        .expect("read")
        .expect("task")
        .history
        .len()
}

/// Criterion: a bind whose committer dies after its decision, before its
/// apply or with only its history appended, converges on the next read to
/// the state an uninterrupted bind leaves: its claim running, its envelope
/// bound, its history appended exactly once, its journal settled and its
/// lease gone. Until then the bundle refuses readers and writers.
#[test]
fn a_bind_interrupted_after_its_decision_converges_to_the_committed_state() {
    let reference = Owner::open(1);
    let admitted = reference.admit("crash");
    let before = history_len(&reference, &admitted.claim.task_id);
    admitted.bind(&reference.boundary);
    let expected = appended(&reference, &admitted.claim.task_id, before);
    assert!(!expected.is_empty(), "a bind appends history");

    for fault in [Fault::BeforeApply, Fault::AfterEvents] {
        let owner = Owner::open(1);
        let admitted = owner.admit("crash");
        let task_id = admitted.claim.task_id.clone();
        let before = history_len(&owner, &task_id);

        apply_fault::set(fault);
        admitted
            .try_bind(&owner.boundary)
            .expect_err("the committer dies in its apply");
        assert_eq!(
            leases(&owner).len(),
            1,
            "{fault:?}: the dead committer leaves its lease"
        );
        owner
            .boundary
            .bundle_store
            .read_bundle_lightweight(&task_id)
            .expect_err("a bundle read is refused until the commit replays");
        owner
            .boundary
            .bundle_store
            .with_bundle_write_lock(&task_id, || Ok(()))
            .expect_err("a bundle write is refused until the commit replays");

        // Any read through the store settles the partition first.
        assert_eq!(
            appended(&owner, &task_id, before),
            expected,
            "{fault:?}: the replay appends the bind's history exactly once"
        );
        let row = owner
            .tasks
            .get_task_row(&task_id, false)
            .expect("read")
            .expect("task");
        assert_eq!(row.task.job_run_id.as_deref(), Some("leaf-crash"));
        assert!(
            owner
                .boundary
                .store
                .unsettled_task_commit_journal(owner.boundary.workspace_id())
                .expect("journal")
                .is_empty(),
            "{fault:?}: the replay settles the journal"
        );
        assert!(
            leases(&owner).is_empty(),
            "{fault:?}: the replay clears the lease"
        );

        // The claim's receipt replays the committed outcome.
        assert_eq!(
            admitted.bind(&owner.boundary).phase,
            ExecutionClaimPhase::Running
        );
        assert_eq!(appended(&owner, &task_id, before), expected);
    }
}

/// Criterion: a host crash can lose a lease's name, which is not durable,
/// while the decision it guarded survives. Opening the partition's boundary,
/// as every process does before it reads, replays that decision.
#[test]
fn opening_a_boundary_replays_a_decision_whose_lease_was_lost() {
    let owner = Owner::open(1);
    let admitted = owner.admit("lost");
    let task_id = admitted.claim.task_id.clone();
    let before = history_len(&owner, &task_id);

    apply_fault::set(Fault::BeforeApply);
    admitted
        .try_bind(&owner.boundary)
        .expect_err("the committer dies before its apply");
    for lease in leases(&owner) {
        std::fs::remove_file(lease).expect("lose the lease");
    }

    let reopened = owner.boundary_with(FileLockOptions {
        timeout: Duration::from_secs(10),
        ..FileLockOptions::default()
    });
    assert!(
        reopened
            .store
            .unsettled_task_commit_journal(reopened.workspace_id())
            .expect("journal")
            .is_empty(),
        "opening the boundary settles the orphaned decision"
    );
    assert!(
        !appended(&owner, &task_id, before).is_empty(),
        "the replay applied the bind's history"
    );
}

/// Criterion: journal retention deletes only applied decisions. A decision a
/// dead committer left committed survives the pass however old it is, and
/// the next read still replays it to the committed state.
#[test]
fn journal_retention_keeps_an_interrupted_decision_for_its_replay() {
    let owner = Owner::open(2);
    let settled = owner.admit("settled");
    settled.bind(&owner.boundary);
    let admitted = owner.admit("crash");
    let task_id = admitted.claim.task_id.clone();
    let before = history_len(&owner, &task_id);
    apply_fault::set(Fault::BeforeApply);
    admitted
        .try_bind(&owner.boundary)
        .expect_err("the committer dies before its apply");

    // Age every row, the unsettled one oldest, so neither its age nor its
    // recency protects it: only its state does.
    let store = &owner.boundary.store;
    let applied_at = (Utc::now() - TimeDelta::days(400)).to_rfc3339();
    let unsettled_at = (Utc::now() - TimeDelta::days(500)).to_rfc3339();
    store
        .connection()
        .lock()
        .expect("store connection")
        .execute(
            "UPDATE task_commit_journal \
             SET created_at = CASE WHEN state = 'applied' THEN ?1 ELSE ?2 END, \
                 applied_at = CASE WHEN state = 'applied' THEN ?1 END",
            [&applied_at, &unsettled_at],
        )
        .expect("age the journal");
    let scope = RetentionWorkspaceScope {
        workspace_id: owner.boundary.workspace_id(),
        legacy_ids: None,
    };
    let plan = store
        .journal_retention_candidates(scope, Utc::now())
        .expect("plan");
    assert!(
        !plan.journal_ids.is_empty(),
        "the settled claim's earlier decisions are selected"
    );
    assert_eq!(
        store
            .delete_applied_journal_rows(&plan.journal_ids)
            .expect("prune"),
        plan.journal_ids.len()
    );
    let unsettled = store
        .unsettled_task_commit_journal(owner.boundary.workspace_id())
        .expect("journal");
    assert_eq!(unsettled.len(), 1, "the interrupted decision survives");
    assert_eq!(unsettled[0].state, TaskCommitJournalState::Committed);
    assert_eq!(unsettled[0].task_id, task_id);

    // The next read replays it exactly as it would have without the pass.
    assert!(
        !appended(&owner, &task_id, before).is_empty(),
        "the replay applied the bind's history"
    );
    let row = owner
        .tasks
        .get_task_row(&task_id, false)
        .expect("read")
        .expect("task");
    assert_eq!(row.task.job_run_id.as_deref(), Some("leaf-crash"));
    assert!(
        store
            .unsettled_task_commit_journal(owner.boundary.workspace_id())
            .expect("journal")
            .is_empty(),
        "the replay settles the journal"
    );
    assert_eq!(
        admitted.bind(&owner.boundary).phase,
        ExecutionClaimPhase::Running,
        "the claim's receipt replays the committed outcome"
    );
    assert_eq!(
        settled.bind(&owner.boundary).phase,
        ExecutionClaimPhase::Running,
        "a claim whose decisions were pruned still replays its receipt"
    );
}
