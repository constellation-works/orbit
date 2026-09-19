---
type: pattern
summary: "Task/Reservation Commit Boundary"
last_validated: 2026-09-19
---
# Task/Reservation Commit Boundary

One durable decision publishes a task transition, its history, a file reservation, and the
dependent coordination rows that go with them — across storage that cannot commit together.

A task's status lives in its bundle files, its projection in the task registry database, and
its reservation in the host store database. Anything that must publish all of them as one fact
(distributed admission is the first such caller) cannot get there by chaining three
independently committed writes, and cannot roll an already-published `task.yaml` back. The
boundary makes the decision in one place and replays everything else from it.

Implementation: `crates/orbit-store/src/repository/task/coordination/` (protocol, serialization,
recovery) over `crates/orbit-store/src/driver/sqlite/task_commit_journal/` (the SQL).

## The two mechanisms

**One serialization boundary per task-store partition.** An advisory lock beside the
partition directory. Ordinary task reads and writes, and ordinary reservation writes, take it
*shared* — they still run concurrently with one another and still take their own per-bundle or
SQLite locks underneath. An admission section takes it *exclusive*, so its readiness reads and
its commit see state no ordinary write can change in between. Every participant takes the
boundary **before** any bundle lock, including `with_task_write_lock`, so the two locks are
always acquired in the same order; the shared helper's per-thread re-entrance makes a nested
task lock inside an admission section run under the outer acquisition rather than deadlock.

**One durable commit decision.** A journal row in the host store database:

| Step | What happens | What a crash here means |
|---|---|---|
| Prepare | Durable pending marker, then a `prepared` journal row carrying the bundle-side intent | Undecided: recovery abandons it; the task is untouched |
| Decide | **Commit point.** One SQLite transaction inserts the reservation and the coordination rows and flips the row to `committed` | Decided: recovery replays the apply |
| Apply | Truncate `events.jsonl` to the intent's recorded pre-apply length, append its events, republish `task.yaml`, settle the row `applied`, drop the marker | Still decided: the next entrant replays it again |

Nothing infers the decision from bundle contents, and nothing tries to un-publish an envelope.
Because no bundle file is touched before the commit point, a pre-commit failure has nothing to
compensate beyond the journal row and the marker.

**Recovery before exposure.** The marker is the cheap signal: one existence check on a read.
Whoever sees it takes the boundary exclusively and settles the journal before exposing any
state — so a committed reservation whose transition has not landed is never observable, and a
live commit blocks a reader for its (short) duration instead of showing it a half-applied task.
A compensation or replay that fails leaves the marker in place and returns the error: the
partition stays closed until recovery succeeds, rather than opening with unknown commit state.

## Integration API

Compose the participants together — the boundary only serializes what holds the same instance:

```rust
let backends = orbit_store::compose::workspace_coordinated_backends(registry, partition_id, store)?;
let boundary = backends.commit_boundary.clone(); // task + reservation backends share it
```

Publish a decision, optionally inside a section that also covers the readiness reads:

```rust
boundary.with_admission(|| {
    // Readiness, dependencies, ordering, conflicts: read through the ordinary
    // task APIs. Nothing an ordinary write could change moves under them.
    let outcome = boundary.commit_task_transition(&TaskCoordinationCommitParams {
        task_id,
        actor,
        expected_status: vec![TaskStatus::Backlog], // compare-and-set inside the boundary
        status: Some(TaskStatus::InProgress),
        status_event: Some("pulled_by".into()),
        status_note,
        append_history,
        reservation: Some(reserve_params), // the task's own canonical footprint
        rows: vec![TaskCoordinationRow { kind, row_id, payload_json }],
    })?;
    Ok(outcome)
})
```

`TaskCoordinationCommitOutcome` is the full result vocabulary: `Committed` (durable, with the
reservation result and journal id), `Stale` (compare-and-set refused), `Conflicted`
(reservation overlap), `RowExists` (a coordination row identity was already published). Only
`Committed` writes anything.

Coordination rows are the generic slot for a caller's own durable coordination records — an
admission receipt, a claim, a tombstone. `(workspace_id, kind, row_id)` is unique, so replaying
a commit under an identity that already exists is refused by the database rather than
duplicated, and `TaskCommitBoundary::coordination_rows` reads them back after recovery settles.
The boundary stores `payload_json` without interpreting it: receipt schema, claim phases,
replay semantics, and retention belong to the caller, not here.

## Composition and cost

`compose::workspace_task_backends` plus `task_reservation_store_sqlite` still compose an
*uncoordinated* pair with exactly the behaviour they always had: per-bundle locking,
independent reservation transactions, no journal, no marker. Storage layout is identical either
way — the coordinated composition adds serialization and recovery, not a new bundle or row
format — so the two can be composed for different partitions on the same host, and a partition
can move between them without migrating data.

Serve a partition through **one** composition at a time. An uncoordinated writer neither takes
the boundary nor sees the pending marker, so a partition served by both at once could have an
ordinary write land between a commit and its replay and be overwritten by that replay.

What coordination costs: one shared advisory-lock acquisition per ordinary task or reservation
operation, one existence check per read, and — during an admission commit — exclusion of that
partition's ordinary writes and of readers that observe the marker. What it does not cost:
concurrency between ordinary writers, which still serialize only per bundle.

## When to reach for it

- **Two storage technologies must publish one fact**, and one of them has no rollback for what
  it already published. Record the decision once; make everything else a replay of it.
- **A decision must be made from state that cannot move under it.** Put the readers and the
  commit in the same exclusive section, and make every ordinary writer take the shared side —
  a lock held only around the final writes does not prevent the check from going stale.

## When NOT to

- **A single-store write.** One SQLite transaction, or the bundle's own `PendingWriteGuard`,
  already gives atomicity with far less machinery.
- **A general distributed-transaction framework.** This boundary is deliberately one shape —
  one task transition plus its reservation and coordination rows — because that shape can be
  replayed deterministically. A generic multi-resource transaction manager cannot.
