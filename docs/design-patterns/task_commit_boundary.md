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

**One serialization boundary per task-store partition, with a host admission lock.**
The host lock excludes ordinary writers across partitions during admission, because a task
may depend on a task in another workspace. Ordinary operations hold it shared; admission
holds it exclusive. The partition advisory lock then protects recovery and bundle publication. Ordinary task reads and writes, and ordinary reservation writes, take it
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
| Decide | **Commit point.** One SQLite transaction inserts reservations/coordination rows, checks and replaces lifecycle rows, releases the settling claim’s reservation, and flips the journal to `committed` | Decided: recovery replays the apply |
| Apply | Truncate `events.jsonl` to the intent's recorded pre-apply length, append its events, replay summary/comments/artifact bytes and manifest, republish `task.yaml`, settle the row `applied`, drop the marker | Still decided: the next entrant replays it again |

Nothing infers the decision from bundle contents, and nothing tries to un-publish an envelope.
Because no bundle file is touched before the commit point, a pre-commit failure has nothing to
compensate beyond the journal row and the marker.

**Recovery before exposure.** The marker signals interrupted publication. Ordinary operations check it again
after acquiring their shared partition lock, so a commit that crashed while they waited cannot
be missed. Recovery drops that shared acquisition and settles the journal exclusively before
exposing any state — so a committed reservation whose transition has not landed is never observable, and a
live commit blocks a reader for its (short) duration instead of showing it a half-applied task.
A compensation or replay that fails leaves the marker in place and returns the error: the
partition stays closed until recovery succeeds, rather than opening with unknown commit state.

## Integration API

Compose the participants together so every instance shares the same locks and decision journal:

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

Internal claim lifecycle uses the same boundary. `ClaimInvocation` is supplied by trusted
runtime code, never deserialized from tool input. Current claim, machine, immutable bound run,
and phase are checked under exclusive serialization; the SQL decision also compares the old
claim/state payloads before replacing them. Mutation receipts deduplicate exact retries. Recovery
revokes authority and refuses unresolved external merge intent; it does not infer failure from age
or reservation expiry. Read-only claim inspection refuses pending repair rather than replaying it.

Journal intent schema 2 includes replayable evidence. Schema 1 remains readable with empty evidence;
older binaries refuse schema 2. Comment replay records the original log length, artifact replay
writes the recorded bytes/manifest, and summary replay writes the recorded content. A crash after
the decision leaves a repair obligation, never a successful partial settlement.

## Composition and cost

Every runtime builder branch uses `workspace_coordinated_backends`, including explicit
data roots, checkoutless partitions, and alternate runtime constructors. The task and
reservation stores come from the same composition; the uncoordinated reservation factory
has been removed. A durable `.task-commit-required` marker makes legacy task compositions
refuse access to a coordinated partition, including compositions opened before activation.
There is no automatic downgrade. Maintenance/import operations must be quiesced separately.

Ordinary operations remain concurrent under shared host and partition locks. Admissions
exclude ordinary writers across the host registry while checking cross-workspace
dependencies. Recovery nesting is tracked per partition rather than by a process-wide depth.

Internal distributed admission builds on this journal. Dependent rows can be finalized from
the reservation result inside the deciding SQLite transaction, so the receipt captures the
actual reservation ID and expiry atomically. Idle receipts use a SQL-only transaction because
there is no task bundle to publish. Receipt compaction replaces the payload with a permanent
tombstone; it never deletes the request identity and refuses unsettled claims.

The public pull/lifecycle tools remain unavailable. Generic task writes refuse active
execution claims until the lifecycle layer supplies a claim-scoped mutation path. Frozen
claim footprints also participate in ordinary reservation conflicts after TTL expiry.

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

Coordinated activation pins the partition to its canonical file-backed journal path.
A second composition using another database is refused; dependency recovery also
checks this binding before reading a foreign partition. Moving a journal requires
an explicit offline migration of its binding, not an implicit reopen.
