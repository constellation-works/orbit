---
title: Task Migration — Vision
owner: claude
last_updated: 2026-09-11
last_validated: 2026-09-11
status: Draft
feature: task-migration
doc_role: vision
type: design
summary: From one-shot export/import to a repeatable owner-wins sync between hosts that each mint under their own task_prefix.
tags: [task-migration, multi-host, task-prefix, sync]
paths: ["crates/orbit-store/src/workflow/task/**", "crates/orbit-cli/src/command/task/**", "crates/orbit-cmd/src/registry_runtime.rs"]
related_features: [task-migration, host-registry, remote-access, federated-mcp]
related_artifacts: [ORB-00034, ORB-10721, ORB-12126]
---

# Task Migration — Vision

Export/import was built for a one-time move ([ORB-00034]). Per-host prefixes
([ORB-10721]) made a second live store safe to *create*; what is still missing
is a way to keep two live stores mutually visible without hand-merging. This doc
sketches that direction. Everything below the open questions is speculation
unless it cites a task.

## 1. Open Questions

1. **Policy or command?** The core deliverable of [ORB-12126] is an import
   policy — working name `--on-conflict owner-wins` — that overwrites a local
   bundle only when the incoming task's prefix differs from the local
   `task_prefix`, and never touches a local-prefix bundle. A convenience wrapper
   (`orbit task sync <ssh-dest>`: export on the peer over SSH, import locally,
   then the reverse) can come later or not at all; scripts already compose the
   two halves. Should the wrapper exist in the binary, or stay a runbook?
2. **Which direction is routable?** The reference fleet is one-way: the laptop
   can reach the box, the box cannot reach the laptop. Any sync therefore runs
   from the laptop side. If a future fleet is fully routable, does the sync
   become a routine on the box, or does it stay operator-driven?
3. **What does a mirror show?** A foreign-prefix task in `orbit task list` is
   read-only. The dashboard and MCP surfaces need to say so — a mutation
   attempt should fail with "owned by `<prefix>`", not silently succeed and
   diverge. Where does that guard live: the store (prefix ≠ local ⇒ refuse
   write), or each surface?
4. **Cross-host relations.** A `DANI-*` task may `depends_on` an `ORB-*` task.
   Import must preserve foreign target ids verbatim (no renumbering), and the
   dependent is only dispatchable once its target's mirror has landed — so the
   sync order is pull-then-push. Is a dangling foreign relation an error, a
   warning, or normal transient state?
5. **Frictions and learnings.** Only the task registry is in scope for
   [ORB-12126]. The global frictions store has the same two-writer shape; the
   docs corpus does not (it is git). Do frictions get the same prefix-ownership
   treatment, or a simpler append-only merge?
6. **Idempotency proof.** Owner-wins must be a no-op on a second identical run
   and must never write an `.idmap.json`. Is byte-identical bundle comparison
   (today's `AlreadyPresent` test) sufficient, or does the manifest need a
   per-task content hash so the comparison is cheap over SSH?

## 2. Prior Work

### Single-writer replication

CouchDB-style multi-master replication needs conflict revisions because any
node may write any document. Orbit avoids the problem rather than solving it:
the prefix is a static partition of the write set, so replication degenerates to
"copy the partitions you do not own". This is the same simplification that
makes per-shard ownership cheap in sharded databases — no consensus, because no
two nodes ever contend for the same key.

### Git remotes

`git fetch` never rewrites local branches; it updates `refs/remotes/*`, which
are read-only mirrors of someone else's history. Owner-wins import is the same
idea applied to task bundles: foreign-prefix bundles are the remote-tracking
refs; local-prefix bundles are your branches.

### Orbit-internal

- [host-registry](../host-registry/2_design.md) owns `task_prefix` and refuses
  a conflicting prefix once allocation has begun.
- [remote-access](../remote-access/1_overview.md) is the route for a client
  that owns no checkout: reach the owner over MCP instead of minting locally.
- [federated-mcp](../federated-mcp/1_overview.md) is the proposed cross-host
  discovery surface; a mirror that is visible but read-only is a natural fit for
  its capability split.

## 3. What May Be Distinctive

Most task trackers solve multi-host by having exactly one host. Orbit's
distinctive move is that ownership rides on the id itself: a reader can tell
from `DANI-00012` alone which machine may write it, and an importer can decide
overwrite-or-skip without consulting any other state. That makes the sync
logic small enough to be obviously correct, at the cost of forbidding the case
where two hosts collaborate on one task — which the [decisions](./4_decisions.md)
accept deliberately.

## 4. References

**Orbit-internal**

- [1_overview](./1_overview.md) — the migration recipe and §5 authority rules
- [4_decisions](./4_decisions.md) — why prefix ownership, and why execute-where-you-mint

**External**

- Git remote-tracking branches — the fetch-never-rewrites-local model this mirrors.

## Task References

- [ORB-00034] — task migration tooling: `orbit task export/import/reindex`.
- [ORB-10721] — per-host `task_prefix` in `host.toml`.
- [ORB-12126] — owner-wins cross-host sync: import policy that overwrites only foreign-prefix bundles.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
