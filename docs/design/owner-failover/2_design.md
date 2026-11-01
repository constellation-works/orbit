---
title: Owner Failover — Design
owner: claude
last_updated: 2026-11-01
last_validated: 2026-11-01
status: Proposed
feature: owner-failover
doc_role: design
type: design
summary: Proposed contract for owner change-log mirroring, debounced git bootstrap, epoch-fenced handoff and takeover, per-epoch task-ID ranges, and old-owner rejoin with an operator conflict list.
tags: [owner-failover, distributed-drain, task-publication, failover, multi-host]
paths: ["crates/orbit-store/src/driver/sqlite/task_registry/**", "crates/orbit-store/src/workflow/task/**", "crates/orbit-cmd/src/registry/**"]
related_features: [distributed-drain, task-publication, host-registry, state-compatibility, federated-mcp]
related_artifacts: [ORB-20620, ORB-12488, ORB-11068, ORB-11074, ORB-11076, ORB-11110, ORB-12528, ORB-13663]
---

# Owner Failover — Design

> **Status: Proposed.** Nothing in this document is implemented. It describes the contract
> Orbit would adopt if Daniel accepts it. Section 10.1 lists the questions only he can answer.

This document proposes how ownership of a shared workspace moves between hosts while each
workspace keeps one writer. Followers hold a read-only mirror of the owner's task records
(§2), git publication bootstraps and backs that mirror up (§3), and an ownership epoch
recorded on the publication branch fences every move (§4). A planned handoff and an unplanned
takeover both use that fence (§5). Each epoch gets its own task-ID range (§6). An old owner
that comes back stands down, and its divergent writes are reconciled (§7). §8 covers
compatibility and rollout, and §9 lists failure behaviour. Ownership of runs, claims,
reservations and logs stays host-local and never moves. The feature has no `3_vision.md`
while it is a proposal. Its open questions end this document because the task asks for them
here.

## 1. Current contracts this design changes

Every row cites the shipped contract and states the proposed change. On acceptance, the cited
docs would be updated in the implementing changes, not in this proposal.

| Contract today | Where | Proposed change |
|---|---|---|
| One owner per repository. A second owner is collapsed by re-registering one checkout as a replica, as a runbook step. | [distributed-drain §1](../distributed-drain/2_design.md#1-roles-one-owner-n-followers) ("Operator precondition") | Still one owner at a time. The owner is now the holder of the highest epoch, and the role flip is a governed command (§5), not a manual re-registration. |
| Tasks, dependencies, comments and history are owner-read and owner-write with "no replica-local fallback". | [distributed-drain §5](../distributed-drain/2_design.md#5-transport-and-authority-routing), authority table | Writes stay owner-only. Reads still route to the owner. A follower also keeps a labelled mirror, readable only on explicit request (§2.4), so the "no fallback" rule holds for every implicit read. |
| A claimed leaf reads its task from the `claimed_task` receipt snapshot, because the executor's local store lacks the task or holds an unrelated record under the same ID. | [distributed-drain §3](../distributed-drain/2_design.md#3-pull-mode-drain-and-the-pulled-leaf-pipeline), "Claimed-leaf crew" | Unchanged. The mirror lives in its own partition, so it never collides with the replica's local records. |
| Manual claim reclamation only: no heartbeat, no failure inference. | [distributed-drain §3.1](../distributed-drain/2_design.md#31-attempt-ownership-and-recovery), "Manual reclamation only" | Unchanged. The mirror head and the poke are freshness reads. Nothing infers death from them. Takeover is operator-triggered (§5.3). |
| Followers check binary and protocol parity through the read-only probe. | [distributed-drain §4.1](../distributed-drain/2_design.md#41-read-only-admission-probe) | The probe also reports the owner's epoch and mirror head. A caller may declare the highest epoch it has seen (§4.3). |
| Only the declared owner publishes, and only that owner can restore. | [task-publication §1](../task-publication/2_design.md#1-ownership-boundary), [§6](../task-publication/2_design.md#disaster-recovery) | Only the epoch holder publishes. A follower may promote its mirror into a canonical store, but only through a fence commit it won (§5). |
| Publication is explicit, plus a weekly routine that ships disabled. No post-write hook. | [task-publication §4](../task-publication/2_design.md#4-snapshot-construction) | On-change debounced publication, still outside task-write acknowledgement (§3.1). |
| A moved branch is an authority conflict. It is never merged or forced. | [task-publication §5](../task-publication/2_design.md#5-compare-and-swap-and-competing-writers), [§8](../task-publication/2_design.md#8-failure-semantics) | Moving within the same epoch is still a conflict. A tip that carries a higher epoch naming another machine deposes the publisher (§4.2). Compaction is the only lease-guarded non-fast-forward update (§3.2). |
| Snapshot history grows monotonically, with no compaction. | [task-publication §9](../task-publication/2_design.md#9-concerns--honest-limitations) | Checkpoint compaction (§3.2). |
| Authority transfer and multi-writer sync are deferred. | [task-publication §9](../task-publication/2_design.md#9-concerns--honest-limitations), [vision](../task-publication/3_vision.md) | Authority transfer is defined here. Multi-writer sync stays rejected: two writers exist only by accident, and their divergence is reconciled, never converged automatically. |

## 2. Ongoing sync on pull and poke

Daniel's option 2, "servers are synced on board meta data on every pull or poke calls", is
the primary channel.

### 2.1 The owner change log and cursor

No usable cursor exists today. `audit_events.id` is an autoincrement over command
invocations, not task state. It is written outside the task commit, and audit is never
published ([task-publication §1](../task-publication/2_design.md#1-ownership-boundary)). The
task commit journal (`task_commit_journal`, [ORB-12528]) is keyed by a text `journal_id` and
has no total order. Bundle files carry no sequence.

The owner therefore adds a `task_change_log` table to the store that holds the task
registry:

| Column | Meaning |
|---|---|
| `workspace_id` | Logical workspace |
| `epoch` | Ownership epoch in which the change committed |
| `seq` | Integer, strictly increasing within `(workspace_id, epoch)`, gap-free |
| `task_id` | Changed task |
| `kind` | `upsert` or `delete` |
| `jsonl_lengths` | Line counts of `events.jsonl`, `comments.jsonl` and each review-thread file after the change |
| `committed_at` | Owner wall clock, diagnostic only |

A row is appended in the same SQLite transaction that commits a task write, including task
creation, field and status updates, relation changes, comment and history appends, artifact
manifest changes and deletion. The cursor is the pair `(epoch, seq)`. It is ordered first by
epoch and then by seq. A cursor from an older epoch is still valid when its epoch ended at a
known seq (§4.1).

The change log is a durable projection kept in step with task writes, so a writer that does
not append to it corrupts the cursor. §8.1 makes this hold for older binaries too.

**Retention.** Each row is about 150 bytes. The owner keeps rows for 30 days or the newest
200,000 rows, whichever is more. That is about 30 MiB at the current rate (§6.1). A cursor
older than the retained floor gets `cursor_expired`, and the follower rebootstraps from git
(§3).

### 2.2 What rides on which call

The mirror *head* rides on every call. The *delta* travels on its own read-only call. This
keeps the pull receipt's stored result and its input digest unchanged.
Distributed-drain [§2](../distributed-drain/2_design.md#2-the-ready-queue-and-orbittaskpull)
replays a stored result for a retried `request_id`. A delta embedded in that result would
replay stale.

- **Head sidecar.** Every response of the five distributed-drain protocol operations (probe,
  receipt lookup, pull, bind and settle) gains `mirror_head: {epoch, seq, published_seq}`.
  The owner computes it fresh at response time, outside the stored receipt.
- **`orbit.mirror.changes`.** A new read-only `control_plane` operation. Input: workspace
  selector, `after: {epoch, seq}`, page limit. Output: one delta page (§2.3). It creates no
  receipt, reservation or claim. Like the other protocol operations, it is served on the
  owner's internal runtime route and omitted from public MCP discovery.
- **Poke.** A poke is a call to `orbit.mirror.changes` that is not prompted by a drain. Its
  triggers are in §2.5.

A follower whose cursor is behind any `mirror_head` it receives fetches pages until it reaches
that head. It does this after its current protocol step, never inside one, so a slow delta
cannot delay a bind or a settle.

### 2.3 Delta format

Deltas are state-based. An entry carries the task's current state, not the operation that
produced it, so applying a page twice is harmless. The retired Task Sync proposal needed
operation replay for its multi-writer registry. This design rejects that registry.

```yaml
owner_epoch: 3
from: {epoch: 3, seq: 1180}
to:   {epoch: 3, seq: 1243}
head: {epoch: 3, seq: 1297}
more: true
epoch_closures: []          # e.g. [{epoch: 2, final_seq: 40211}] when the page crosses an epoch
tasks:
  - task_id: ORB-20620
    seq: 1243               # newest change-log row for this task in the page
    kind: upsert
    task_yaml: <canonical task.yaml bytes>
    prose: {description, acceptance, plan, execution_summary}
    appends:                # lines after the length recorded at the follower's cursor
      events: [...]
      comments: [...]
      review_threads: {<thread>: [...]}
    artifact_manifest: <manifest.yaml bytes>   # hashes and sizes, no file bytes
    coordination_facts: [...]                  # §2.4, append-only
```

- The owner coalesces each task to its newest state within the page. It reads the bundle
  under the canonical bundle lock, the lock publication already uses
  ([task-publication §4](../task-publication/2_design.md#4-snapshot-construction)), so an entry
  is whole-old or whole-new.
- Appends start after the JSONL lengths recorded in that task's newest change-log row at or
  before the caller's cursor. The owner therefore needs no per-follower state. A follower that
  finds a length mismatch discards that task's mirror copy and asks for it again from cursor
  zero for that task (`refetch: [task_id]`). The same refetch closes an open publication hole
  (§3.3) when the owner is reachable. It does not run during an outage, and it does not rewrite
  git. A hole still open at takeover is copied onto the fence; rejoin, not a later refetch of
  the old publication, recovers it.
- A page holds at most 200 tasks or 1 MiB, the inline limit the protocol already uses. A task
  larger than a page arrives alone, with its prose split across continuation pages.

### 2.4 What the mirror holds and what stays host-local

| Mirrored (task-scoped, the owner's canonical record) | Host-local (never mirrored, never published) |
|---|---|
| `task.yaml`: fields, status, tags, crew, dependencies and relations | Job runs, steps, checkpoints, logs |
| Description, acceptance, plan, execution summary | Execution claims, claim receipts, admission receipts |
| `events.jsonl` (history), `comments.jsonl`, review threads | Reservations, path leases, context locks |
| Artifact manifests, plus file bytes fetched in the background by hash (below) | Handoff and landing rows, release budgets |
| Once-per-head CI rerun facts (`ci_check_rerun_reserved`, `ci_check_rerun`) as append-only coordination facts | Executor census, audit events, indexes (rebuilt locally from the mirror) |
| Task deletions as tombstones | Host registry, destinations, runtime configuration |

The CI rerun facts are the one coordination record mirrored. They record a consumed,
once-per-head budget, not a lease. A new owner that lacked them could send a second rerun for a
head the old owner already retried
([distributed-drain §3.2](../distributed-drain/2_design.md#32-durable-review-and-landing-handoff),
`ci_check_rerun_reserved`).

Frictions are excluded in this version (question 7 in §10.1).

**Attachment bytes.** The ws_orbit corpus holds about 2.4 GiB of attachments against 167 MiB
of core records (§6.1), so deltas carry manifests only. A follower fetches missing blobs in
the background with `orbit.mirror.artifact` (read-only, by content hash, newest tasks first,
bandwidth-capped). A blob the mirror lacks at takeover is reported as omitted, as an
incomplete publication's attachments are
([task-publication §7](../task-publication/2_design.md#7-privacy-and-attached-artifact-policy)).

**Where the mirror lives.** It is a separate task-registry partition, `mirror:<workspace_id>`,
on the follower. Normal runtime selection never picks it for a checkout, so implicit reads keep
routing to the owner. Explicit reads (`orbit task show --source mirror`, and the same field
on `orbit.task.show` and `orbit.task.list`) return mirror records labelled
`{source: mirror, epoch, seq, fetched_at}`, never as live state. The mirror is read-only. Only
`orbit.mirror.changes`, the git bootstrap and takeover promotion write it.

### 2.5 Staleness and keeping an idle follower fresh

| Follower state | Mirror staleness bound |
|---|---|
| Drain running | Each refill pass calls the probe, so lag is at most one refill interval plus the time to fetch pages |
| Idle, owner reachable | The poke interval. The default is 5 minutes, set by `authority.mirror.poke_interval` |
| Owner unreachable | Frozen at the last successful page. The label records `fetched_at` |
| Just started | Unknown until the startup sync (below) finishes. Explicit mirror reads report `syncing` |

The poke runs from a follower-local routine, `owner_mirror_refresh`, seeded on every replica
checkout of a workspace whose owner is epoch-aware. It is cheap when nothing has changed: one
call returns an empty page and the head. It is a freshness read only. Failed pokes are logged
and counted, but they never mark the owner dead and never trigger anything (§5.3).

**Startup sync.** This is Daniel's "When each server starts up, they sync via the git repo".
When a host starts its Orbit clock or daemon, it does the following for each workspace it
holds a replica of:

1. Fetch the publication branch tip and read its envelope (§4.1).
2. If the mirror is empty, belongs to an older epoch whose closure it lacks, has an expired
   cursor, or sits behind the envelope's `change_seq`, import the publication tree into the
   mirror. It uses the validation of the restore path
   ([task-publication §6](../task-publication/2_design.md#disaster-recovery)), not its
   destination rules. Then set the scalar cursor to `(owner_epoch, change_seq)` and record
   one open hole for each `withheld` entry (§3.3). A cursor with open holes is not caught up.
   `change_seq` does not mean every task is current through that seq. Import does not replace
   a mirror copy that this follower already refetched from the owner at or after that task's
   `withheld_seq`; that copy stays and its hole stays closed.
3. Poke the owner for the delta from that scalar cursor, and refetch every holed task (§2.3).
   A successful refetch stores the task in the mirror and closes that hole. Those bytes come
   from the owner, not from the publication tree, so denied prose stays unpublished. An
   unreachable owner leaves the holes open. A later refetch cannot repair a takeover that
   already promoted the incomplete tree; the fence carries the holes instead (§5.2, §7.2).
4. Run the epoch gate (§4.3) before starting any drain.

## 3. Cold start and git bootstrap

Git publication is the bootstrap and backup channel. It fills an empty mirror, survives the
owner's disk, and carries the fence (§4). It is not the sync channel. Section 2 handles sync.

### 3.1 Frequency: on change, debounced

The owner publishes when unpublished change-log rows exist and either of these holds:

- the head has been quiet for `authority.publication.quiet` (default 2 minutes); or
- the oldest unpublished row is `authority.publication.max_lag` old (default 15 minutes).

A periodic owner-local job compares the change-log head with the last published `change_seq`.
A hole whose `withheld_seq` is already below that `change_seq` does not make the head look
unpublished: the commit that advanced past the hole carries it in `withheld` (§3.3). The job
is not a post-write hook, so a task write is still acknowledged by the local store alone.
The decision
[Publication does not participate in task-write acknowledgement](../task-publication/4_decisions.md#publication-does-not-participate-in-task-write-acknowledgement)
stands. When nothing has changed, nothing is published. Byte-identical reruns already create
no commit ([task-publication §4](../task-publication/2_design.md#4-snapshot-construction)).
Publication rate depends on the write pattern, so no single figure bounds it:

- Continuous writes (each within `quiet` of the last) never leave the head quiet, so the
  max-lag trigger publishes once per `max_lag`: 96 a day at the 15-minute default
  (86,400 s ÷ 900 s). That is the rate for this write pattern, not a ceiling.
- Sparse writes let the quiet trigger fire after each burst. A publication needs unpublished
  rows, and those were written after the previous publication. So a quiet-triggered publication
  comes more than `quiet` after the one before it, and a max-lag one more than `max_lag`. At the
  defaults, that caps publications at about 720 a day (86,400 s ÷ 120 s). Writes every 181
  seconds (at 0, 181, 362 seconds and so on), with a publisher that checks every second, publish
  at 120, 301, 482, 663 seconds and so on: about 477 a day, above 96.

The cap holds for any check interval. Getting close to it needs a check about every second, so
a publication fires as soon as the quiet window closes; a coarser interval publishes less often.
These counts cover change-triggered publications only. Checkpoint compaction (§3.2) and other
owner commits are extra. They are estimates from the trigger rules, not measurements.

The weekly routine is retired in favour of this job.

### 3.2 Bounding repository growth

**Growth estimate for ws_orbit.** Core bundles average about 17 KiB (167 MiB over 9,752
tasks). 2,642 distinct tasks had history appended in the 7 days before 2026-10-31, about 380 a
day on average. Every published change writes a new blob for each changed file of each changed
task, and a JSONL append rewrites the whole file blob. A task therefore gets one new blob per
publication that changes it, so growth depends on how many publications touch each task, not
only on how many tasks change. Estimated new blob bytes per day are 380 × v × 17 KiB ≈
6.3 MiB × v, where v is the assumed average number of published versions per changed task per
day. v = 1 gives the 6.3 MiB figure and holds only when each changed task is published once that
day. v is an assumption, not a measurement: v = 3, for example, gives about 19 MiB a day before
compression. The rules bound versions only loosely, at most one version per changed task per
publication. At the sparse-write cap in §3.1 (about 720 publications a day) and 380 tasks, that
bound is about 4.4 GiB a day, too loose to size compaction against. Git's zlib and delta packing
typically shrink text like this several times over. With v = 1 that gives an estimated 1–2 MiB a
day, or roughly 0.4–0.7 GiB a year packed; packed size scales roughly with v at the same ratio.
This is an estimate, not a measurement: no on-change lineage exists yet. Debouncing coalesces
repeated edits to one task within a window, which keeps v down.

**Checkpoint compaction.** When packed history exceeds
`authority.publication.compact_ratio` (default 4) times the packed size of the live tree,
the owner compacts:

1. Build a root commit holding the current snapshot, with no Git parent. Its envelope keeps
   `generation`, sets `segment: n + 1`, and records `segment_predecessor: <old tip oid>`.
2. Push the old tip to `refs/orbit/archive/<publication_id>/segment-<n>` (absent-ref
   compare-and-swap).
3. Update the bound branch from the observed old tip to the new root with an exact
   expected-object lease (`--force-with-lease=<ref>:<old oid>`). This is the only
   non-fast-forward update Orbit ever makes. It stays a compare-and-swap, so a competing
   writer still fails it.
4. Delete archive refs older than `authority.publication.archive_segments` (default 2).

On Git hosts whose branch protection refuses every non-fast-forward update, compaction is
disabled and reported, and growth stays the operator's limitation. Deleting refs shrinks
clones immediately. Host-side storage shrinks only when the Git host collects garbage.

### 3.3 Redaction at higher frequency

Publication moves from weekly to within 15 minutes, so a secret pasted into a comment reaches
the remote quickly and, as Git history, durably
([task-publication §7](../task-publication/2_design.md#7-privacy-and-attached-artifact-policy)).
The proposal adds:

- **Attachments omitted by default** for the on-change job, using the existing
  `publication.routine_attachments = omit`. Mirror attachment transfer (§2.4) does not depend on
  publication.
- **Deny patterns over core prose** (description, plan, summary, comments and history
  messages), not only attachment paths. A match does not publish that task's new prose.
  The publication tree keeps the task's previously published bytes, or omits the task when
  it has never been published. The rest of the publication proceeds, so one comment does not
  stop every backup. The owner raises an operator notice. Denied prose is never written to a
  publication commit, a fence commit or an archive ref.
- **A withheld task is a hole under `change_seq`.** Each withheld entry is
  `{task_id, reason, epoch, withheld_seq}`. `withheld_seq` is the newest change-log row for
  that task, in `epoch`, that this snapshot does not include. It can be lower than
  `change_seq` in the same epoch. `change_seq` stays the newest row whose resulting state
  the tree includes for some task (§4.1), and it still advances when a later row for a
  different task is included. It is not a completeness watermark. An empty `withheld` list
  means the snapshot is complete through `change_seq`. A non-empty list is an **incomplete
  snapshot**. Importing one sets the scalar cursor to `(owner_epoch, change_seq)` and records
  one open hole per entry. Adopting that scalar does not recover the hole. Startup, takeover
  and rejoin carry the hole until the owner serves the task or rejoin applies it (§2, §5.2,
  §7.2). A later epoch that copies the hole forward keeps the epoch the row was withheld in.
  The new epoch's own `change_seq` does not close it.
- **No retraction promise.** Debouncing is not a retraction window. Once a commit is pushed,
  the existing rule applies: history rewrite and credential rotation are operator incidents.

## 4. The ownership epoch and the fence commit

### 4.1 Envelope additions

The publication envelope
([task-publication §3](../task-publication/2_design.md#3-tree-and-manifest-contract)) moves to
`format_version: 2` with these fields:

```yaml
owner_epoch: 3
authority_machine_id: hm_4bdeb191f194f07e     # the epoch's owner, as today
change_seq: 1297                              # newest included change-log row; not a completeness watermark (§3.3)
fence: none | handoff | takeover              # set only on the commit that starts the epoch
epochs:
  - {epoch: 1, owner: hm_9ca6004473492f06, id_base: 1}
  - {epoch: 2, owner: hm_9ca6004473492f06, id_base: 1, final_seq: 40211}
  - epoch: 3
    owner: hm_4bdeb191f194f07e
    id_base: 23000
    started_by: takeover
    base_cursor: {epoch: 2, seq: 40198}       # scalar cursor the new owner adopted
    base_holes: []                            # withheld tasks at or below that cursor (§3.3, §7.2)
    prior_id_ceiling: 22999                   # highest ID the prior epoch may hold without renumbering
segment: 1
withheld: []                                  # [{task_id, reason, epoch, withheld_seq}] when incomplete
```

The `epochs` list is append-only. A fence commit adds exactly one entry and closes the
previous one. A handoff records the closing `final_seq` and leaves `base_holes` empty,
because the target has already refetched every withheld task (§5.1). A takeover records
only what the taker knew: `base_cursor`, plus one `base_holes` entry for every open hole
in that base. The old owner's real final seq is unknown until rejoin. A scalar
`base_cursor` does not cover a task named in `base_holes`. `withheld_seq` on a hole can
be lower than `change_seq`.

### 4.2 The fence

A fence commit is an ordinary publication commit with `fence` set and `owner_epoch`
incremented by one. It is pushed with the same exact compare-and-swap as every publication
([task-publication §5](../task-publication/2_design.md#5-compare-and-swap-and-competing-writers),
[ORB-11110]). Of two competing fence attempts against one observed tip, exactly one succeeds.
The loser changes nothing locally.

The publisher's rejection rule gains one branch:

| Fetched tip after a failed compare-and-swap | Meaning | Behaviour |
|---|---|---|
| Same epoch, different commit | Another writer in this epoch | Authority conflict, as today |
| Higher epoch, owner is another machine | This host was deposed | Enter deposed mode (§7.1) |
| Higher epoch, owner is this machine | A handoff to this host completed | Finish promotion (§5.1, step 7) |
| Lower epoch, or the envelope does not match the binding | Repository tampering or a binding error | Binding conflict, as today |

### 4.3 The epoch gate on every host

Every host keeps a local `authority_epoch` record per workspace. It holds the epoch it last
accepted and the owner that epoch names. A host acts as owner only when it holds the highest
epoch it has seen. It checks this at three points:

- **Startup.** Before serving writes, drain clocks or mutating MCP tools, fetch the branch tip
  with a bounded timeout (default 10 seconds).
- **Every publication attempt.** Under §4.2.
- **Every probe from a follower.** The probe accepts an optional declared `caller_known_epoch`.
  An owner that learns of a higher epoch this way deposes itself before answering. The
  follower then gets typed `owner_deposed {owner_machine_id, epoch}`.

Followers refuse to pull from, bind to, or settle with an owner whose `mirror_head.epoch` is
lower than the highest epoch they have seen. They then require their destinations file to name
the new owner's machine. If it does not, they report `owner_destination_missing` naming the
machine. Orbit does not rewrite host configuration.

When the branch is unreachable at startup, the host trusts its local record and warns. §10
names this limitation.

## 5. Taking over ownership

All commands below are operator-only CLI commands under `orbit workspace authority`, matching
the publication commands' placement under the workspace noun
([task-publication §2.1](../task-publication/2_design.md#21-operator-surface)). Operator MCP
tools may mirror them later. Managed agents cannot call them.

### 5.1 Planned handoff (no data loss)

`orbit workspace authority handoff --to <machine> --confirm` runs on the current owner:

1. **Preflight.** The target is reachable through its destination, runs an epoch-aware build
   with the same protocol fingerprint, is a replica of this workspace, and has a mirror at the
   current epoch with all attachment blobs fetched.
2. **Quiesce.** The owner stops admission: pull returns `owner_handing_off`, and drains end
   their windows. Owner-local and follower claims settle normally. The command waits
   `--wait` (default 30 minutes) for zero live claims in the `claimed`, `running`,
   `handed_off` and `repair_pending` phases. If claims remain, it stops and lists them. The
   operator lands, settles or revokes each through the existing recovery commands
   ([distributed-drain §3.1](../distributed-drain/2_design.md#31-attempt-ownership-and-recovery)).
   Claims are never transferred (§7.3).
3. **Freeze.** All task writes are refused with `owner_frozen`. The change-log head `H` is now
   fixed.
4. **Final publish** at the current epoch with `change_seq = H`. A task whose prose is still
   denied is withheld (§3.3). That commit does not contain the denied prose.
5. **Target catch-up.** The owner calls the target's `orbit.mirror.ack` over the target's
   destination. The target fetches pages from its own cursor through `H`. It does not replace
   that cursor with the final publication's `change_seq` while a hole is open. It refetches
   every task the final envelope withholds. The refetch travels on the mirror channel, so the
   denied prose reaches the target's mirror and not the Git commit. The target returns the
   digest of its mirror's core bundles after those refetches. The owner compares it with its
   own digest and stops on any mismatch or any hole that is still open. No fence is pushed
   in that case.
6. **Fence.** The owner pushes the fence commit: `owner_epoch + 1`, `fence: handoff`,
   `authority_machine_id = <target>`, `final_seq = H`, `base_holes` empty, and `id_base`
   equal to its allocator's next number. No buffer is needed because writes are frozen.
7. **Role flip.** The old owner records the new epoch, flips its checkout to a replica of the
   target and keeps its canonical partition as its mirror at cursor `(E, H)`. The target sees
   the fence on its next poke or branch fetch, or because the old owner notified it. It
   promotes its mirror into its canonical partition, advances its allocator to `id_base`, and
   flips its checkout to owner.

**Data-loss window: none.** Writes stop at step 3, and the target proves at step 5 that it holds
every write up to `H`, including tasks the final publication withheld. A hole the target cannot
refetch stops the handoff, so a withheld update is not dropped on the way past the fence. Step 6
records `H` in the fence with `base_holes` empty. A crash before step 6 leaves the old owner as
owner. Its startup unfreezes it, and the operator reruns the handoff. A crash after step 6 is
finished from the branch tip: §4.2 promotes the target and deposes the old owner with an empty
divergent set. Host-local runs, logs and audit stay on the old host. They are not lost, and
they are not moved.

### 5.2 Unplanned takeover

`orbit workspace authority takeover --confirm --reason <text>` runs on a follower:

1. **Preconditions.** The checkout is a replica of the workspace. The owner probe fails with a
   transport error; the command refuses when the owner answers and points to handoff. The last
   publication is `format_version: 2`, so the old owner understands fences (§8). The local
   canonical partition for the workspace is empty, which is the restore rule
   ([task-publication §6](../task-publication/2_design.md#disaster-recovery)).
2. **Choose the base.** Fetch the branch tip `T` at epoch `E`. If the mirror is at epoch `E`
   and its scalar cursor is at or past `T.change_seq`, the base is the mirror, including any
   open holes it still has (§3.3). Otherwise import `T` into the mirror first. Import does not
   replace a copy the mirror already refetched at or after that task's `withheld_seq`. Every
   remaining withheld entry stays an open hole. The base cursor is `(E, S)`, where `S` is
   that scalar cursor. Open holes are not covered by `S`. A newer mirror is not replaced by
   an older git tip just because a hole is open.
   Takeover is not refused because a snapshot is incomplete. The fence records the holes, and
   rejoin recovers them (§7.2). The promoted tree still holds the previously published bytes,
   so the denied prose is not published by the takeover commit.
3. **Size the ID range** (§6.2). The command prints the computed `id_base` and
   `prior_id_ceiling` and accepts `--id-buffer` to override them.
4. **Fence.** Build a snapshot from the mirror with `owner_epoch: E + 1`, `fence: takeover`,
   the taker as `authority_machine_id`, and the new `epochs` entry. That entry carries
   `base_cursor: {epoch: E, seq: S}` and `base_holes` copied from the open holes. The fence
   envelope keeps those tasks in `withheld`, so a follower that imports the fence does not
   treat the published bytes as current. Later publications of the new epoch keep listing
   an unresolved hole until rejoin applies it or the operator resolves the hold. Compare-and-swap
   the fence against `T`. On failure, stop. Nothing local has changed. A moved tip means the
   owner or another taker is alive.
5. **Promote.** Move the mirror into the canonical partition through the restore path's staged,
   rolled-back import ([ORB-11076]). Advance the allocator to `id_base` with the existing
   `bump_allocator_to_at_least`. Bind the workspace's task prefix to this host for the
   partition. Flip the checkout to owner. Record epoch `E + 1`.
6. **Hold inherited execution.** Every task the base shows as `in-progress` or `review` has no
   claim on the new owner, because claims are never mirrored. Such a task gets the reconcile
   hold `orphaned_by_takeover`. Every task named in `base_holes` gets `withheld_at_takeover`,
   whether or not it was in progress. Pull does not admit a held task and landing does not
   land it. For a hole, the bytes in the tree are the last published version, not the withheld
   update. `withheld_at_takeover` stays until rejoin applies that update or records a conflict,
   or until the operator resolves the hold knowing the old store is gone (§7.4). An
   `orphaned_by_takeover` hold stays until the operator recovers the task (§7.3).
7. **Repoint followers.** Other followers learn the new epoch from the branch at their next
   startup, branch fetch or poke failure. They then use §4.3.

**Data-loss window.** The new owner lacks every write the old owner committed after the base
cursor `(E, S)`, and every write named in `base_holes` even when its seq is at or below `S`.

- **Included visibility loss (until rejoin).** The writes in `(S, old head]` for tasks the base
  includes. In time, this is the mirror staleness at takeover (§2.5): at most one refill
  interval with a drain running, at most the 5-minute poke interval when idle, and no more
  than the old owner's last publication lag (15 minutes at most) when the base comes from a
  complete git snapshot. A takeover from a complete snapshot therefore misses at most about
  15 minutes of included writes when it could reach git, and usually under 5. This bound does
  not cover a withheld task.
- **Withheld updates (until rejoin).** A deny match keeps the new prose out of every snapshot
  for as long as the match lasts. That can be indefinite, so `withheld_seq` can sit below
  `change_seq` with no time limit. The base records the gap as a hole. The published bytes
  are not the acknowledged update. Rejoin recovers the hole from the old owner's store
  (§7.2, §7.4). There is no shorter bound.
- **Split-brain writes.** An old owner that is alive but cut off from the taker keeps accepting
  writes until it learns of the new epoch. That happens at its next publication attempt, within
  `max_lag` of its next write when it can reach git, or at a probe from a follower that knows
  the epoch. If it is cut off from git and from every follower, the period has no bound, and
  those writes wait for rejoin.
- **Permanent loss.** Only if the old owner's store never comes back. Then the included
  visibility window and every still-open withheld hole are lost for good, along with
  attachment blobs the mirror had not fetched and every host-local record on that host.
  Resolving `withheld_at_takeover` without the old store is that acknowledgement for the
  holed task. It does not invent the withheld prose.

Writes from the included window, the withheld holes and the split-brain window are recovered
at rejoin (§7). Some arrive as conflicts. Denied prose is recovered into the canonical store
over `orbit.authority.reconcile` and is not published (§3.3). A later snapshot withholds it
again when the deny pattern still matches.

### 5.3 Automatic takeover

**Proposed: never automatic in this version.** Takeover is operator-triggered.
Distributed-drain already decided that the owner cannot tell a dead follower from an
unreachable one
([Followers pull; the owner never places](../distributed-drain/4_decisions.md#followers-pull-the-owner-never-places)).
The same holds the other way: a follower cannot tell a dead owner from a partition.

Automatic takeover is safe only with **owner self-fencing**. The owner holds a lease that it
renews by publishing, and it refuses task writes once it cannot renew within the lease minus a
clock-skew margin. A follower may take over only after the lease plus the margin has passed
with no renewal, and only if its own probe also fails. A workable setting would be renewal
every 2 minutes, a 10-minute lease, a 2-minute margin, and takeover allowed after 14 minutes
without renewal. Writes on the owner would then depend on the Git host being reachable. That
reverses
[Publication does not participate in task-write acknowledgement](../task-publication/4_decisions.md#publication-does-not-participate-in-task-write-acknowledgement),
and a Git-host outage would stop every owner. This proposal does not take that trade. It is
open question 1.

## 6. Task IDs across the gap

Daniel's option 1 was "add task_id buffer, .e.g. add +100 to task_id sequence. And when the
downed server comes back online, it will add the tasks in-between the buffer". This design
keeps the idea and replaces the fixed +100 with a per-epoch ID range sized from data.

### 6.1 Observed ws_orbit creation rate

Measured on 2026-10-31 on the owner, dk-server-2 (`hm_9ca6004473492f06`), from the ws_orbit
store through `orbit tool run orbit.task.list` (`created_at` of all 9,752 retained tasks,
ORB-00017 to ORB-20620, created 2026-05-13 to 2026-10-31). "IDs" counts allocator advance,
including IDs whose tasks were later deleted. "Tasks" counts retained tasks only.

| Window | Tasks created | IDs consumed | Per day |
|---|---|---|---|
| Last 7 days | 2,312 | — | 330 tasks |
| Last 30 days | 6,458 | 6,929 | 215 tasks, 231 IDs |
| Last 90 days | 8,966 | — | 100 tasks |

The busiest sliding windows in the last 30 days, by ID span:

| Window | 1 h | 24 h | 72 h | 7 d |
|---|---|---|---|---|
| Peak IDs | 128 | 544 | 1,314 | 2,657 |

The rate is rising, from about 100 tasks a day over 90 days to 330 a day over the last week. A
fixed +100 is less than the busiest single hour (128) and about 10 hours at the 30-day
average.

### 6.2 Sizing rule: one range per epoch

When epoch `E + 1` starts, the fence commit records:

- `H`: the highest task number the new owner knows, from its mirror, the publication, or its
  own allocator high-water mark, whichever is highest.
- `B = ceil(1.5 × R(W))`. `R(W)` is the peak ID consumption over any sliding window of
  length `W` within the last 30 days, computed from the mirror's `created_at` at takeover time.
  `W` is the longest split-brain period the range must absorb, set by
  `authority.takeover.id_window` (default 72 hours, a long-weekend outage of a host that is cut
  off but still running its own drain). The factor 1.5 allows for the rising trend.
- `id_base(E + 1) = roundup_to_1000(H + B + 1)`.
- `prior_id_ceiling = id_base(E + 1) − 1`.

With today's numbers, `R(72 h) = 1,314`, so `B = 1,971`, and `H = 20,620` gives
`id_base = 23,000`. The old owner can mint ORB-20621 to ORB-22999 (2,379 IDs) without a
collision. That is about 10 days at the 30-day average or 5.4 days at the peak 72-hour rate.
In the common case, the old owner is simply down, mints nothing, and the gap stays empty.

Why a range rather than a fixed +100:

- The fence commit records the range, so both sides know the ceiling, and the rejoin
  detects overflow instead of colliding silently.
- Task numbers are `u32` (`ORB_TASK_ID_MAX = u32::MAX` in
  `crates/orbit-types/src/task/artifacts.rs`), so skipping a few thousand IDs per takeover
  costs nothing.
- Rounding to a thousand makes an epoch boundary visible in the ID itself.
- A planned handoff needs no range: `id_base` is the frozen allocator's next number (§5.1).

The workspace's task prefix (`ORB-`) moves with the epoch. The new owner mints `ORB-` IDs for
this workspace, not its own host prefix, because tasks are identified by workspace prefix and
the epoch holder is the minting authority. The task-migration rule "the minting host owns each
task" now reads: the epoch holder owns the tasks.

**Overflow.** An old-epoch task numbered at or above `id_base(E + 1)` is renumbered at rejoin
into the new owner's range. It keeps a `renumbered_from` relation and goes on the conflict list
(§7.2), because commits or PRs may cite the old ID. Overflow renumbers whether or not the new
owner has used the number yet, so the two epochs' IDs never interleave.

## 7. The old owner rejoining

### 7.1 Standing down

At startup (§4.3), on its next publication attempt (§4.2) or at a probe from a follower that
knows the epoch, the old owner sees a higher epoch that names another machine and enters
**deposed** mode for that workspace:

- Task mutation surfaces refuse with typed `owner_deposed {owner_machine_id, epoch}`. The
  check runs inside the write transaction against the local `authority_epoch` row, so
  long-lived MCP processes are refused too, not only new ones.
- Drain clocks, routines and the landing consumer for that workspace do not start, or they
  stop at their next pass. No run resumes. Other workspaces on the host are unaffected.
- Reads still work and are labelled `deposed`.

It never drains as owner again without a new fence that names it.

### 7.2 Reconciling divergent writes

`orbit workspace authority rejoin --confirm` runs on the deposed host:

1. **Divergent set.** The change-log rows of epoch `E` after the fence's `base_cursor.seq`,
   plus every task named in the fence's `base_holes`. A hole is a withheld update at or below
   that cursor. The scalar range does not select it. The hole is what puts the task in the
   set. The deposed host sends that task even when the range is empty. If the range and the
   holes are both empty, skip to step 4. A task that appears in both is sent once.
2. **Rejoin bundle.** For each divergent task, the deposed host sends its current state in the
   delta format (§2.3). A base hole is sent from the task's current store record, not from a
   retained change-log row: log retention (§2.1) can drop the row without dropping the task.
   If that task record itself is gone, the bundle names the hole as
   `withheld_update_unavailable` and does not send the published bytes as if they were
   current. The bundle also adds live claims, runs, handoffs and preserved branches as
   `orphaned_claim` records (§7.3), and the CI rerun facts.
3. **Apply on the new owner.** The new owner applies the bundle through the operator-only
   `orbit.authority.reconcile`. It is idempotent by
   `(old machine, old epoch, highest seq among the divergent rows and the hole withheld_seqs)`.
   The base version is the task in the fence commit's tree. When a hole still has published
   bytes, that base is the previous version, not the withheld update. Prose fields (description,
   acceptance, plan and execution summary) follow the field rule below. They are not
   append-only. A hole whose task is absent from the fence tree is imported with the same ID
   when its number is within
   `prior_id_ceiling`. That task was withheld, not minted after the base. The rules:

   | Old-side change since base | New-side state since base | Result |
   |---|---|---|
   | New task, number ≤ `prior_id_ceiling` | — | Imported with the same ID |
   | New task, number ≥ `id_base` | — | Renumbered (§6.2), conflict entry |
   | Comment, history event, review-thread entry | Anything | Union. Duplicates are dropped by entry identity (operation ID or event ID), or by content digest when an entry has none. Ordered by timestamp. Never rewritten |
   | CI rerun fact | Anything | Union |
   | Field, tag, relation or status change | Unchanged | Applied, except a status change into or out of `in-progress` or `review`, which is a conflict entry because it reflects a claim that no longer exists |
   | Field, tag, relation or status change | Also changed | Conflict entry: both versions and the base kept. Never last-writer-wins |
   | Deletion | Anything | Conflict entry: Orbit never deletes automatically |

   Every applied change gets a history event with `origin: rejoin`, the old epoch and the old
   machine. Applying a hole does not write the payload to the publication branch. When the
   applied prose still matches a deny pattern, the next publication withholds it again (§3.3).
   Followers of the new owner refetch it from that owner when the owner is reachable. An
   applied hole clears `withheld_at_takeover`. A conflict result replaces that hold with
   `authority_conflict`. `withheld_update_unavailable` is an unresolved conflict: the
   published bytes stay labelled incomplete, and the hold is not cleared by guessing.
4. **Retire and follow.** The deposed host renames its canonical partition to
   `retired:<workspace_id>:e<E>`, read-only and kept for audit. It flips its checkout to a
   replica of the new owner and rebuilds its mirror from git and the delta.

**The conflict list.** These are durable owner-side `authority_conflict` rows: task,
kind, base, old and new versions, origin epoch and machine, and resolution. A task with an
open conflict carries the reconcile hold `authority_conflict`. Pull does not admit it and
landing does not land it. The operator resolves each entry with
`orbit workspace authority conflicts --resolve <entry> --take old|new` or by editing the task.
That makes an ordinary audited operator write and closes the entry. The dashboard shows a
conflict panel, and the matching operator MCP tool lists and resolves entries. Agents can read
the list, but cannot resolve entries.

### 7.3 In-flight work across the epoch boundary

Execution state belongs to the epoch that admitted it. It never crosses an epoch boundary.

| In flight at the outage | What happens |
|---|---|
| **Old owner's own runs and claimed leaves** | They stop with the host. On a deposed restart, nothing resumes. Worktrees, branches and PRs stay. Rejoin reports each as `orphaned_claim {task, claim, run, branch, pr, head}`. The new owner records a conflict entry. The task already carries `orphaned_by_takeover` (§5.2, step 6). The operator re-queues it as a new claim on the new owner or closes the PR. |
| **Old owner's open PRs** | PR state lives on the Git host and is shared. The new owner has no handoff or landing record for them, so it never lands them automatically. A PR that merged during the outage shows up in the conflict entry. The operator marks the task `done` after checking the merge, as with an external merge today. |
| **Pending landing handoffs** (`handed_off`, `repair_pending`) | Same as above. Handoff acceptance and landing intent are host-local coordination rows. |
| **Once-per-head CI rerun reservations** | Mirrored as facts (§2.4), so the new owner inherits the consumed budget up to its base. A reservation whose reply was unknown arrives as reserved and is never resent. Reservations the old owner made after the base union in at rejoin. Worst case: one extra rerun of a head both owners acted on during split-brain. |
| **Follower claims held against the old owner** | The follower's leaf finishes. Its settle call fails, and the durable pending settlement retries idempotently ([ORB-13663]). Once the follower knows the new epoch, it sends the settlement to the new owner. The new owner answers typed `claim_from_prior_epoch` because it never had the claim. The follower then files the candidate as an `orphaned_claim` report, which creates a conflict entry and changes no task, and stops retrying. If the old owner comes back first, it refuses the settlement with `owner_deposed` naming the new owner. |
| **A taker's own in-flight leaf** | Same as a follower claim. A claim admitted by epoch `E` cannot settle against epoch `E + 1`, even on the same host. |
| **Old owner's reservations, path leases and admission receipts** | They die with the epoch and are retired with the partition. The new owner starts with none. Receipts stay unique because the new owner's receipt namespace is scoped by epoch. |

Re-queueing an orphaned task later might adopt its preserved branch instead of starting over.
That is a separate design. This proposal only preserves the evidence.

### 7.4 Worked example: a withheld update below the snapshot cursor

Epoch `E`. The owner has acknowledged both writes locally.

1. Task A changes at seq 41. Its new prose matches a deny pattern. No publication includes
   that prose. The change log still holds the row.
2. Task B changes at seq 42. B's prose is clean. The debounced publication runs. The tree
   contains A's previously published bytes and B's seq-42 state. The envelope has
   `change_seq: 42` and
   `withheld: [{task_id: A, reason: prose_deny, epoch: E, withheld_seq: 41}]`.
   A's seq-41 prose is not in the commit.
3. **Git bootstrap.** The owner is unreachable. A fresh follower imports that tip. Its scalar
   cursor becomes `(E, 42)` and it records an open hole `(A, 41)`. It does not treat A as
   current. The refetch cannot run, so the hole stays open.
4. **Takeover.** The follower takes over. Its mirror was just imported and sits at
   `(E, 42)` with the hole still open, so the base is that tip. The fence records
   `base_cursor: {epoch: E, seq: 42}` and
   `base_holes: [{task_id: A, reason: prose_deny, epoch: E, withheld_seq: 41}]`.
   Promotion copies the tree, including stale A, and does not publish the denied prose. A receives
   `withheld_at_takeover`. Pull will not admit A and landing will not land it. B is current.
5. **Rejoin.** The old owner returns with head 42 and stands down. The divergent set is the
   rows of epoch `E` with seq greater than 42, which is empty, plus the fence's base holes,
   which is A. The deposed host sends A's current store record. The new owner has not changed
   A, so the field rule applies A's seq-41 prose and clears `withheld_at_takeover`. The apply
   writes a history event with `origin: rejoin`. The payload is not published. If the new
   owner had edited A, both versions and the stale published base would be kept as an
   `authority_conflict`, and that hold would replace `withheld_at_takeover`. Either way the
   seq-41 update is not dropped.
6. **After rejoin.** If A's prose still matches the deny pattern, the next snapshot withholds
   it again. Followers refetch A from the new owner when that owner is reachable. Git still
   does not store the denied prose.
7. **Bound.** B's seq-42 state is in the tree, so the base includes B and rejoin does not
   have to recover it. The 15-minute figure in §5.2 is only for included writes, and only
   when the snapshot is complete. It does not cover A. A's withheld update can sit below
   `change_seq` for as long as the deny match lasts. Rejoin recovers it. It is permanent
   only if the old store never returns.

## 8. Compatibility and rollout

### 8.1 Store migration and long-lived clients

The change log and the `authority_epoch` row are a projection kept in step with task writes,
so the migration is `ReadCompatible`
([state compatibility §1](../state-compatibility/2_design.md#1-migrations-declare-their-own-compatibility)).
Older binaries open the store read-only
([§4](../state-compatibility/2_design.md#4-read-only-is-enforced-not-promised)). A process
that opened the store before the migration does not reopen it, and its writes would skip the
change log. The implementation must close that gap in one of two ways. Picking between them
is a prerequisite:

- **Trigger-maintained log.** A SQLite trigger on the registry row that every task write
  updates (`task_bundle_index`, or another row if not every write touches that one) appends the
  change-log row. Old writers then produce correct rows. This must first be verified for every
  write path, comment-only appends included.
- **Refusing trigger.** Follow the v57 handoff-settlement precedent
  ([state compatibility §9](../state-compatibility/2_design.md#9-handoff-settlement-attribution-and-older-lifecycle-readers)).
  Triggers refuse task-row writes that do not carry the new writer's change-log append in the
  same transaction. An already-open older writer then gets a refusal and rolls back, so no
  write is lost silently.

The deposed check (§7.1) also runs inside the write transaction, so a long-lived MCP process
cannot write past a fence.

### 8.2 Protocol and mixed versions

- New operations (`orbit.mirror.changes`, `orbit.mirror.artifact`, `orbit.mirror.ack`,
  `orbit.authority.reconcile`) are additive. Existing protocol request types do not change, so
  the type-derived request fingerprint does not either
  ([distributed-drain §4.1](../distributed-drain/2_design.md#41-read-only-admission-probe)).
  The one optional request field, `caller_known_epoch`, goes on the probe declaration, which
  already accepts caller declarations.
- The owner emits the `mirror_head` response field only to callers whose probe declared a
  version that understands it. Older followers see unchanged responses.
- A follower without mirror support keeps working under today's contract. It cannot take
  over, and it learns a new epoch only when its owner's probe fails.
- Takeover refuses unless the last publication is `format_version: 2`. An old owner that
  predates fences would read the fence as a plain authority conflict. It would stop publishing
  but would not depose itself, and would go on accepting writes.
- Older publication consumers refuse `format_version: 2` with the existing "unsupported newer
  publication schema" failure
  ([task-publication §8](../task-publication/2_design.md#8-failure-semantics)).
- Hosts deploy in lockstep, as today (polaris RB-15). Takeover is enabled per workspace only
  once every host holding a checkout of it runs an epoch-aware build.

### 8.3 Phased rollout

| Phase | Ships | Authority change |
|---|---|---|
| 1 | Change log, `mirror_head`, `orbit.mirror.changes`, mirror partition, poke routine, startup sync | None. Measure mirror lag and change-log volume |
| 2 | Envelope v2, on-change debounced publication, prose deny patterns, compaction | None. Measure repository growth against §3.2 |
| 3 | Epoch gate, deposed mode, planned handoff | Planned moves only |
| 4 | Takeover, ID ranges, rejoin, conflict list | Unplanned moves |

Each phase is useful alone. Phase 1 gives followers offline task reads. Phase 2 gives a fresh
backup.

### 8.4 Keeping Orbit generic

Nothing here names a host, a workspace or a toolchain. Every threshold is workspace
configuration under `[authority]`: `mirror.poke_interval`, `publication.quiet`,
`publication.max_lag`, `publication.compact_ratio`, `publication.archive_segments` and
`takeover.id_window`. Followers find owners by machine ID through their existing destinations
file. The design works for any workspace with a publication binding. A workspace without one
cannot fail over, and its behaviour is unchanged.

## 9. Failure semantics

| Failure | Behaviour |
|---|---|
| Owner unreachable during a poke | Mirror frozen and labelled. Failure counted. No takeover, no claim change |
| Cursor older than retention | `cursor_expired`. Rebootstrap from git, including its holes (§3.3), then the delta |
| JSONL length mismatch in a delta | Refetch that task whole. Never splice |
| Publication hits a prose deny match | That task's new prose is withheld and recorded as a hole (§3.3). The previous bytes stay in the tree. The operator gets a notice. Denied prose is not in the commit. Takeover copies the hole into `base_holes`; rejoin recovers it |
| Handoff catch-up leaves a hole | Stop before the fence. The owner stays owner. A withheld update is not dropped |
| Compaction refused by branch protection | Compaction disabled and reported. Normal publication continues |
| Handoff preflight or catch-up digest mismatch | Stop before freezing, or unfreeze. The owner stays owner |
| Crash during handoff before the fence | The old owner unfreezes on restart. Rerun the handoff |
| Crash after the fence | Finished from the branch tip on each host's next gate |
| Takeover while the owner answers the probe | Refused. Use handoff |
| Takeover compare-and-swap loses | Nothing changes locally. The owner or another taker is alive |
| Old owner restarts with Git unreachable | Acts on its local epoch record and warns (§10) |
| Settlement for a claim from an older epoch | `claim_from_prior_epoch`. The candidate becomes an `orphaned_claim` conflict entry |
| Rejoin replayed | Idempotent by `(machine, epoch, highest seq)`, including hole `withheld_seq` values |

## 10. Concerns & Honest Limitations

- **The Git host is a failover dependency.** Takeover and handoff need the publication
  repository. Ordinary owner writes do not. If the owner and the Git host are both down, no
  failover is possible.
- **Partitions are reconciled, not prevented.** An old owner cut off from Git and from every
  follower keeps acting as owner for its local clients until it can see the fence. If it
  restarts while still cut off, it trusts its local record. Its writes come back at rejoin, some
  as conflicts. Its IDs can overflow the range after about 5–10 days at today's rates and are
  then renumbered.
- **A takeover loses visibility of the old owner's last few minutes of included writes**
  (§5.2) until the old owner rejoins, and loses them for good if its disk never returns.
  A task withheld by a prose deny pattern is outside that bound. It can be missing from
  git for as long as the pattern matches. Rejoin recovers it from the old store (§7.4).
  A store that never returns loses it.
- **Execution history does not move.** Runs, logs, audit and claims stay on the host that
  produced them. After a takeover, the new owner's dashboard and run inspection start empty
  for the old epoch.
- **Every follower holds the full task prose.** Followers already have SSH owner access in
  this single-user model, so this widens no trust boundary. It does widen the set of disks
  that hold the data.
- **The conflict list can be long** after a long split-brain period, and held tasks wait for
  the operator.
- **The growth numbers are estimates** (§3.2). Phase 2 must measure them before compaction
  thresholds are trusted.
- **Change-log coverage must be proven** for every task write path (§8.1). A path that misses
  the log silently desynchronizes followers until their next rebootstrap.
- **A laptop can take over** but should not stay owner. The decision
  [The owner is the always-on host that followers can reach](../distributed-drain/4_decisions.md#the-owner-is-the-always-on-host-that-followers-can-reach)
  still governs the steady state. A takeover by the Mac is an outage measure, followed by a
  planned handoff back.
- **Status vocabulary.** The task asked for status `Proposed`, which
  [CONVENTIONS §7](../CONVENTIONS.md#7-status-lifecycle-per-doc) does not define. The index
  generator does not validate status. On acceptance these docs move to `Accepted`.

### 10.1 Open questions for Daniel

1. **Automatic or operator-triggered takeover?** The proposal is operator-only (§5.3).
   Automatic takeover needs owner self-fencing, which makes every owner write depend on the
   Git host. Is an outage that lasts until you run one command acceptable, or should a lease
   (for example 10 minutes) trade write availability for unattended failover?
2. **Sync frequency.** Are the defaults right: a 5-minute idle poke, publication after a
   2-minute quiet period or 15 minutes at most, and a delta on every drain call? Tighter
   values shrink the included-write window (§5.2) and cost more owner reads and more Git
   commits. They do not bound a withheld hole. That lasts until the deny match clears or
   rejoin recovers it.
3. **Conflict-review surface.** CLI plus dashboard panel plus operator MCP tool (§7.2). Is the
   dashboard panel enough, or should conflicts also reach you as decision items through the
   human-decision channel?
4. **Handoff with live claims.** Handoff waits for claims to settle or makes you revoke them
   (§5.1). Is a 30-minute wait acceptable, or should handoff move the claims too? That would
   require mirroring claim state, which this design avoids.
5. **ID window.** Should `W` be 72 hours (`id_base` = 23,000 today), or long enough to cover a
   week-long split-brain period (`R(7 d)` = 2,657, giving `id_base` = 25,000)?
6. **Which hosts may take over?** Any replica, or only hosts on an allowlist? The Mac sleeps
   and loses its OAuth session.
7. **Frictions.** Should friction records mirror and fail over with tasks? They have their own
   allocator, so they would need their own ID range.
8. **Retire the weekly routine?** On-change publication replaces it (§3.1). Should a
   workspace without failover keep the weekly option?

## Task References

- [ORB-20620] — wrote this proposal.
- [ORB-20639] — made a withheld update below `change_seq` recoverable at rejoin.
- [ORB-12528] — landed the task commit boundary the change log would join.
- [ORB-11076] — implemented the staged, rolled-back restore that takeover promotion reuses.
- [ORB-11110] — made publication an exact compare-and-swap, the fence this design relies on.
- [ORB-13663] — made follower settlements durable and idempotently retried.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
