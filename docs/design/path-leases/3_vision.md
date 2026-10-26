---
title: Path Leases — Vision
owner: claude
last_updated: 2026-10-25
last_validated: 2026-10-25
status: Draft
feature: path-leases
doc_role: vision
type: design
summary: "Open questions for path leases (lease units, honesty, release detection, who the listener is for) and prior work: Perforce and Git LFS locking, coordination-service leases, and merge queues."
tags: [path-leases]
paths: []
related_features: [conflict-budget, remote-access]
related_artifacts: []
---

# Path Leases — Vision

Forward-looking notes for path leases. Everything here is open; the proposal itself is
[2_design.md](./2_design.md).

## 1. Open Questions

1. **Should advisory leases price drain admission at all?** Counting them in the conflict
   budget's `holders(path)` is cautious, but an advisory holder that rarely lands anything
   inflates risk. Weighting by each principal's measured landing rate is proposed for later.
2. **Who is the listener for?** Phase 4 is justified only by principals without SSH: teammates,
   cloud agents, CI. If none appear, the SSH surfaces are the whole feature.
3. **Is file granularity enough?** The conflict budget's symbol replay
   ([conflict-budget §5](../conflict-budget/1_scope.md#5-symbol-level-locking-as-an-experiment))
   answers this for tasks; leases should follow whatever it finds rather than add their own
   granularity.
4. **Can footprints be inferred for leases?** A session could lease what its first edits touch
   and widen as it goes. The measurements say inference is not a precondition (about 40% of
   recovered conflicts trace to undeclared files), but it would make advisory leases much more
   accurate.
5. **Should landing notices reach drain tasks?** A running task whose base moved under a hot path
   could rebase early instead of at landing. That changes delivery, not admission, and belongs in
   a delivery design if it is wanted.
6. **One notice stream or one per lease?** A workspace-wide sequence makes SSE resumption
   simple; per-lease streams would be cheaper to filter for clients with many leases.

## 2. Prior Work

### File locking in version control

Perforce exclusive checkout (`+l` file types) and Git LFS file locking (`git lfs lock`) make a
path exclusive to one user until unlocked. Both are pessimistic and opt-in per file type, and
both are disliked when applied broadly, which is why most teams use them only for unmergeable
binaries. Path leases borrow the opt-in shape (hard only by path policy or request) and avoid the
broad case by defaulting to advisory.

### Coordination-service leases

Chubby, ZooKeeper ephemeral nodes and etcd leases tie a lock to a session kept alive by
heartbeat, so a dead client's lock expires. The TTL-plus-renew lifetime and the cursor-based
watch (etcd's revision-based watch resumption) are the direct models for §4 and §5.

### Merge queues

GitHub merge queue, Bors and similar tools serialize *landing*: they test each change on top of
the queue ahead of it. They act after the work is done; leases act before it starts. The two
compose: a lease reduces how often a queued change fails to rebase.

## 3. What May Be Distinctive

- **One lock index for agents in drains and agents outside them.** Most tools coordinate either
  CI-driven landing or human file locks, not both against the same index.
- **Measured from Orbit's own records.** Conflict, denial and footprint-accuracy data already
  exist on the owner, so the cost of a lease policy (unnecessary waits against avoided repairs)
  can be read rather than guessed.
- **Advisory by default.** The useful part for most work is the landing notice, not the lock.

## 4. References

**Orbit-internal**

- [conflict-budget/1_scope.md](../conflict-budget/1_scope.md)
- [distributed-drain/4_decisions.md](../distributed-drain/4_decisions.md)
- [remote-access/1_overview.md](../remote-access/1_overview.md)
- [federated-mcp/4_decisions.md](../federated-mcp/4_decisions.md)

**External**

- Perforce Helix Core, file type modifier `+l` (exclusive open).
- Git LFS, `git lfs lock` and the file locking API.
- Burrows, "The Chubby lock service for loosely-coupled distributed systems", OSDI 2006.
- etcd documentation, leases and watch revisions.
- GitHub documentation, managing a merge queue.

## Task References

None yet; the design is unfiled.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
