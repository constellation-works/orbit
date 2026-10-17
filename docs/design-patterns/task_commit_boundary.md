---
type: pattern
summary: "Task/Reservation Commit Boundary"
last_validated: 2026-10-17
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
The host lock, always taken before the partition lock, excludes ordinary writers across
partitions while an admission reads a dependency that another workspace's partition holds.
Ordinary operations, claim mutations, commits, and an admission whose decision reads only its
own partition hold it shared, so they never stall another partition's work. Only that
cross-partition admission holds it exclusive. Holding it exclusively for every admission
section stalled every workspace's task reads and writes behind one drain's claims. The
partition advisory lock then protects recovery and bundle publication. Ordinary task reads and writes, and ordinary reservation writes, take it
*shared* — they still run concurrently with one another and still take their own per-bundle or
SQLite locks underneath. An admission section takes it *exclusive*, so its readiness reads and
its commit see state no ordinary write can change in between. Every participant takes the
boundary **before** any bundle lock, including `with_task_write_lock`, so the two locks are
always acquired in the same order; the shared helper's per-thread re-entrance makes a nested
task lock inside an admission section run under the outer acquisition rather than deadlock.

Guarded task deletion re-reads eligibility under `with_task_write_lock` and holds that
lock through bundle deletion. Its earlier status check is only a preflight: a task that
became active meanwhile is preserved unless the caller explicitly forces deletion.

Both boundary locks prefer a waiting exclusive acquirer. An admission, recovery or
cross-partition admission refused by shared holders queues at the lock's turnstile
(`<lock>.turnstile`) until it takes the lock. An ordinary section passes through that turnstile
before trying the lock, so it waits behind a queued writer while the sections already inside
drain. Without that priority, a writer got the lock only at an instant when no reader held it,
so a stream of overlapping ordinary sections held admissions past the 3 s warning (ORB-15107).
A section nested inside another partition's section skips the turnstile. It already holds a
partition that the queued writer may be waiting behind indirectly, and waiting behind that
writer could close a cycle. A status-only observation (routine status, through
`TaskCommitBoundary::observe_for_status`) also skips it: it decides and writes nothing, and
queueing each of its dozens of reads behind every admission and commit made one status pass
take seconds under drain load. It still takes the shared locks and settles a pending commit
first, so it never sees a half-applied transition and still waits for an exclusive holder; a
queued writer may wait for the few reads it enters meanwhile. A re-entrant acquisition never
reaches either lock.

A pass that makes many task reads (a search, the config view's review section) enters one shared
section around all of them with `TaskStoreBackend::in_read_section`. Its nested reads re-enter
that section, so the pass queues, takes the locks and writes its holder record once instead of
once per read. Inside it, a pass must not wait on another thread's task reads or request an
admission: either would wait behind the shared lock the pass itself holds.

**One durable commit decision.** A journal row in the host store database:

| Step | What happens | What a crash here means |
|---|---|---|
| Prepare | Name the task's commit lease, `<partition>/.task-commit-applying/<task>`: a file the committer keeps locked until its apply finishes, under the task's bundle lock, which it took before reading the bundle and keeps as long. Then a `prepared` journal row carrying the bundle-side intent | Undecided: recovery abandons it; the task is untouched |
| Decide | **Commit point.** One SQLite transaction inserts reservations/coordination rows, checks and replaces lifecycle rows, releases the settling claim’s reservation, and flips the journal to `committed`. Then `task.yaml` is published without syncs, so lock-free envelope readers see the decided status | Decided: recovery replays the apply |
| Apply | After the admission section has released the partition, still under the bundle lock and lease: truncate `events.jsonl` to the intent's recorded pre-apply length (nothing to do, and no sync, when it is already there), append its events in one write and one sync, replay summary/comments/artifact bytes and manifest, republish `task.yaml` durably, settle the row `applied`, update the registry index, then unname the lease and release the bundle | Still decided: the next entrant replays it again |

Nothing infers the decision from bundle contents, and nothing tries to un-publish an envelope.
Because no bundle file but `task.yaml` is touched before the apply, and that only after the
commit point, a pre-commit failure has nothing to compensate beyond the journal row and the
lease.

Only the prepare and the decision exclude the partition. Every file sync a commit requests is
in its apply, after the section, so a writer to another task in the partition waits for one
task's decision, not for its publication. Under concurrent claim
mutations with real syncs on ext4 the exclusive hold fell from p50 ~24 ms to ~1.2 ms
(`exclusive_claim_sections_stay_short_under_concurrent_durable_mutations`, run on demand).
A thread that commits a task twice in one section applies the first commit before the second
reads the bundle.

**Recovery before exposure.** A dead lease signals an interrupted commit. A live committer holds
its lease's lock from before it prepares until it has applied, and the kernel drops a dead
process's locks but not its files, so a lease nobody holds belongs to a commit that crashed.
Every entrant checks the partition's leases: a store read before it reads, an ordinary section
once it holds the partition shared, an admission once it holds it exclusively. Finding a dead
one, it drops any shared acquisition and settles the journal exclusively. An undecided row is
abandoned, a decided one is replayed under the task's bundle lock, and a row whose lease is
still held is left to its live committer. A bundle reader or writer that meets a dead lease
under the bundle lock is refused until that replay, so a half-applied task is never read or
written over. A live apply blocks full bundle reads of its own task for its duration, and no
other task's reads or partition sections. Listings that already tolerate a bundle in flight
may show the decided envelope while its history is still being applied.

The lease's name is not synced, which keeps every sync out of the section. A host crash can
therefore lose a lease whose decision survived, so opening a partition's boundary, which every
process does before it reads, replays any unsettled row whose lease nobody holds. A
compensation or replay that fails leaves the lease dead and returns the error: the partition
stays closed until recovery succeeds, rather than opening with unknown commit state.

Older versions held a `.task-commit-pending` marker for a whole commit inside the section.
Recovery still honors and clears one they left, waiting for the partition as a reader first.
Their readers and writers do not check leases, so only this version refuses a bundle that a
dead lease guards.

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
claim/state payloads before replacing them. Mutation receipts deduplicate exact retries; a
retry replays its receipt in an ordinary section, so it does not exclude the partition's
readers. A check across claims (a leaf run bound to another attempt, a task's release budget)
asks SQL for the lifecycle rows that mention the run or task and decodes only those, never the
partition's whole claim history, inside the exclusive section. Recovery
revokes authority and refuses unresolved external merge intent; it does not infer failure from age
or reservation expiry. Read-only claim inspection refuses pending repair rather than replaying it.

Journal intent schema 2 includes replayable evidence. Schema 1 remains readable with empty evidence;
older binaries refuse schema 2. Comment replay records the original log length, artifact replay
writes the recorded bytes/manifest, and summary replay writes the recorded content. A crash after
the decision leaves a repair obligation, never a successful partial settlement.

Before preparing the journal, claimed evidence checks the combined artifact manifest for
file/ancestor conflicts and verifies that existing destination parents are directories and
replacement destinations are files. These checks run under admission and bundle locks, before
any artifact writes or commit lease, so an impossible artifact topology cannot create a
durable replay obligation. Nested files and replacements retain the same recovery protocol.

## Composition and cost

Runtime builder branches use `workspace_coordinated_backends`, including explicit
data roots, checkoutless partitions, and alternate runtime constructors. Observation-only
opens use `workspace_observational_backends` when a read-only command's partition is absent
or coordination setup encounters a filesystem permission denial. This includes writable
runtime opens before CLI tool dispatch: an absent partition reads as empty, and an actual
mutation still fails at its storage write. The observation handle creates no partition,
lock, or journal marker during construction and retains journal-binding verification and
recovery before exposure. The task and
reservation stores come from the same composition; the uncoordinated reservation factory
has been removed. A durable `.task-commit-required` marker makes legacy task compositions
refuse access to a coordinated partition, including compositions opened before activation.
There is no automatic downgrade. Maintenance/import operations must be quiesced separately.

Ordinary operations remain concurrent under shared host and partition locks. An admission
section excludes its own partition's writers. It excludes writers across the host registry
only while a decision checks a cross-workspace dependency. A partition-scoped section reaching
such a dependency stops before it writes anything, and the decision is taken again in a
host-wide section. Recovery nesting is tracked per partition rather than by a process-wide
depth.

Owner admission applies the shared completed-archive dependency rule to status history
from canonical bundles inside that lock, after recovering each registered dependency owner.
An archived prerequisite counts as done only when its readable history reached done and
then remained done or archived. Queue depth and claim selection use the same projection;
the prerequisite's stored status stays archived. Abandoned or reopened archives, rejected
or missing prerequisites, and histories without completion remain unsatisfied. Damaged
event logs abort admission without changing a dependent or creating a claim.

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
  a lock held only around the final writes does not prevent the check from going stale. Keep
  that section proportional to the one decision: select candidates before it, under the shared
  side, and inside it re-read and re-check only what the chosen candidate's decision rests on,
  so a stale selection can defer a candidate but never admit it (pull admission, ORB-14724).

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
