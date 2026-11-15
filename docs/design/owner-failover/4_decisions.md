---
title: Owner Failover — Decisions
owner: claude
last_updated: 2026-11-01
last_validated: 2026-11-01
status: Proposed
feature: owner-failover
doc_role: decisions
type: design
summary: Proposed standing rules for owner failover — epoch-fenced authority moves, operator-triggered takeover, cursor mirroring, per-epoch ID ranges, kind-based reconciliation, and execution state that never crosses an epoch.
tags: [owner-failover, distributed-drain, task-publication, failover, multi-host]
paths: ["crates/orbit-store/src/driver/sqlite/task_registry/**", "crates/orbit-store/src/workflow/task/**"]
related_features: [distributed-drain, task-publication, state-compatibility]
related_artifacts: [ORB-20620]
---

# Owner Failover — Decisions

Record non-obvious decisions here by title. Task references carry provenance; superseded decisions remain in place so their original reasoning stays legible. See [CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule and required `Cost:` line.

Every entry below is **proposed**. None is in force until Daniel accepts the design. No code
exists yet, so every entry is a Door 2 rule that governs future decisions. Each entry names the
existing decision it keeps, refines or would supersede. Those existing entries are left
unchanged until an implementing change lands.

## Ownership moves only through an epoch-fenced publication commit

**Recorded:** 2026-10 · [ORB-20620] (proposed)

### Context

Both existing designs hold that exactly one machine writes a workspace's tasks. Failover
needs that machine to change without a moment when two hosts both believe they own the
workspace and both succeed in committing that belief. Orbit has no consensus service. The one
linearizable primitive every host can already reach is the publication branch's exact
compare-and-swap push ([task-publication §5](../task-publication/2_design.md#5-compare-and-swap-and-competing-writers)).

### Decision

Any change of a workspace's owner, planned or not, takes effect only as a fence commit that
raises the ownership epoch by one and wins the publication branch's compare-and-swap. A host
acts as owner only while it holds the highest epoch it has seen. Seeing a higher epoch that
names another machine deposes it. No other signal confers ownership: registry role, SSH
reachability, repository write access or a recent mirror.

### Consequences

- Of two competing takeovers, at most one succeeds. The loser changes nothing.
- This keeps
  [Publication never creates a second mutation authority](../task-publication/4_decisions.md#publication-never-creates-a-second-mutation-authority).
  That decision required a defined authority transfer before offline writes, and this is that
  transfer.
- On acceptance this refines
  [A moved publication branch is an authority conflict](../task-publication/4_decisions.md#a-moved-publication-branch-is-an-authority-conflict).
  Movement within one epoch stays a conflict. A higher-epoch tip deposes.
- Cost: a workspace without a reachable publication repository cannot fail over at all, and
  the Git host becomes a dependency of every ownership move.

## Takeover is operator-triggered; reachability never implies death

**Recorded:** 2026-10 · [ORB-20620] (proposed)

### Context

A follower cannot tell a dead owner from a network partition, just as the owner cannot tell
a dead follower from an unreachable one
([Followers pull; the owner never places](../distributed-drain/4_decisions.md#followers-pull-the-owner-never-places)).
Automatic takeover is safe only if the owner fences itself when it cannot renew a lease. That
makes owner writes depend on the lease medium.

### Decision

An unplanned ownership move starts only from an explicit operator command. Failed pokes,
missing head sidecars, expired reservations and probe timeouts may inform the operator. They
never trigger a takeover, revoke a claim or depose an owner. Any future automatic path must
add owner self-fencing first and accept that write availability then depends on the lease
medium.

### Consequences

- This keeps distributed-drain's "Manual reclamation only" rule
  ([§3.1](../distributed-drain/2_design.md#31-attempt-ownership-and-recovery)) and extends it
  from claims to ownership.
- The mirror head and the poke stay freshness reads. They are not heartbeats.
- Cost: drains on a workspace stop from the moment its owner dies until an operator runs
  takeover. With nobody at the keyboard, the outage lasts until someone returns.

## Followers mirror the owner's change cursor; the mirror is never implicit authority

**Recorded:** 2026-10 · [ORB-20620] (proposed)

### Context

A follower needs current task state to take over, and to read tasks while the owner is down.
Git-only sync is weekly today and debounced at best, so it is too stale to bound the takeover
window. A shared filesystem cannot carry SQLite's locks or WAL. Answering ordinary reads from
the mirror would quietly break distributed-drain's "no replica-local fallback" rule
([§5](../distributed-drain/2_design.md#5-transport-and-authority-routing)).

### Decision

The owner orders every committed task change in a change log. Followers pull state-based
deltas from a cursor on calls they already make, plus a timed poke. Mirrored records are
served only to an explicit mirror read, always labelled with their cursor and fetch time.
Implicit reads and every write still route to the owner.

### Consequences

- Every follower holds a recent, verifiable copy that it can promote, and the takeover
  window is the mirror's staleness, not the backup's.
- State-based deltas apply idempotently. The design never replays operations.
- Cost: every task write path must append to the change log in its own transaction, and one
  that misses it silently desynchronizes followers until their next rebootstrap.

## Each epoch owns a disjoint task-ID range sized from measured peak demand

**Recorded:** 2026-10 · [ORB-20620] (proposed)

### Context

After a takeover, the old owner may have minted IDs the new owner never saw. Any of them may
already be cited in commits, PRs or other tasks. A fixed +100 buffer is smaller than one
peak hour of ws_orbit's measured creation (128 IDs). Separate per-host prefixes would split one
workspace's IDs across prefixes and break the rule that a workspace has one prefix.

### Decision

When an epoch starts by takeover, its fence commit records an ID base of at least the highest
known ID plus 1.5 times the peak ID consumption over the configured split-brain window,
rounded up to a thousand. The prior epoch keeps every ID below that base. An ID the prior
epoch minted at or above the base is renumbered at rejoin and flagged, never merged into the
new range. A planned handoff, with writes frozen, starts at the allocator's next number.

### Consequences

- In the common case, the old owner is down, mints nothing and keeps its IDs. After a long
  split-brain period, the overflow is detectable and bounded.
- The epoch is readable from the ID.
- Cost: every takeover skips thousands of IDs, and overflow renumbering breaks external
  citations of the renumbered IDs, which need the `renumbered_from` relation to resolve.

## Divergent writes reconcile by kind and never last-writer-wins

**Recorded:** 2026-10 · [ORB-20620] (proposed)

### Context

A rejoining old owner may hold writes from after the takeover base, and it may hold a
withheld update from at or below that base when the snapshot did not include it
([2_design.md §3.3](./2_design.md#33-redaction-at-higher-frequency) and
[§7.2](./2_design.md#72-reconciling-divergent-writes)). The new owner may have changed the
same tasks. Timestamps come from two clocks and say nothing about intent.
The retired Task Sync proposal needed operation replay and structured conflict resolution to
merge writers. That is the machinery this design avoids.

### Decision

Reconcile divergent task records by the kind of data. Append-only records (comments,
history, review-thread entries, consumed budgets) union. A field or status change made on
one side only applies, except changes that reflect execution state. A change made on both
sides, any deletion, and any change that reflects a claim go to an operator conflict list.
The affected task is held from admission and landing until each entry is resolved. Neither
side's timestamp ever decides a winner. A publication withhold is not a discard. The update
stays in the old owner's store and joins the divergent set even when its seq is at or below
the takeover base cursor. The reconcile payload is not published, so denied prose stays out
of git.

### Consequences

- No acknowledged write is discarded without an operator seeing it.
- Most rejoins are fully automatic, because the usual old-owner divergence is new tasks and
  comments.
- Cost: held tasks wait for the operator, and a long split-brain period can leave a long
  conflict list.

## Execution state never crosses an epoch

**Recorded:** 2026-10 · [ORB-20620] (proposed)

### Context

Claims, admission receipts, reservations, handoff and landing rows and runs are fenced by the
owner store that admitted them
([distributed-drain §3.1](../distributed-drain/2_design.md#31-attempt-ownership-and-recovery)).
Mirroring them would turn every follower into a replica of the coordination journal and its
fencing guarantees, not just of task records.

### Decision

Execution and coordination state belongs to the epoch whose owner admitted it. A new epoch
inherits none of it. Work that was in flight surfaces as held tasks and orphaned-claim
evidence for the operator to re-queue, land or close. The one exception is a consumed
once-per-head budget fact. It is mirrored as append-only data, because inheriting it can only
prevent a duplicate side effect.

### Consequences

- Fencing stays local to one store. No claim can be honoured by two owners.
- A planned handoff must drain or revoke live claims first.
- Cost: an unplanned takeover strands every in-flight run, so work may be redone, and the new
  owner's run history and dashboard start empty for the prior epoch.

## Task References

- [ORB-20620] — proposed these decisions with the owner-failover design.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
