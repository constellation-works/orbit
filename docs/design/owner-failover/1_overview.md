---
title: Owner Failover — Overview
owner: claude
last_updated: 2026-10-31
last_validated: 2026-10-31
status: Proposed
stage: designing
next: "Daniel reviews the proposal and answers its open questions (ORB-20620)"
feature: owner-failover
doc_role: overview
type: design
summary: Proposed owner failover — followers mirror owner task changes on every drain call, git publication bootstraps them, and an epoch-fenced handoff or takeover moves ownership while the old owner rejoins as a follower.
tags: [owner-failover, distributed-drain, task-publication, failover, multi-host]
paths: ["crates/orbit-store/src/driver/sqlite/task_registry/**", "crates/orbit-store/src/workflow/task/**", "crates/orbit-cmd/src/registry/**"]
related_features: [distributed-drain, task-publication, host-registry, state-compatibility, federated-mcp]
related_artifacts: [ORB-20620, ORB-12488, ORB-11068, ORB-11077]
---

# Owner Failover — Overview

> **Status: Proposed.** Written for Daniel's review. Nothing here is built or approved to
> build. Daniel scoped this task to the design only, with no implementation follow-ups filed.

Today one host owns each shared workspace, and drains stop when that host is down. Every
follower reads and writes tasks through it, with no replica-local fallback
([distributed-drain §5](../distributed-drain/2_design.md#5-transport-and-authority-routing)).
This feature keeps that single writer and makes it movable. Followers keep a read-only mirror
of the owner's task records, refreshed on the calls they already make. Git task publication
becomes the bootstrap and backup channel. An operator can hand ownership to a follower as a
planned move, or let a follower take it over during an outage. The old owner then comes back
as a follower without losing or duplicating work. The design is in
[2_design.md](./2_design.md) and the reasoning is in [4_decisions.md](./4_decisions.md).

## 1. Motivation

Daniel, 2026-10-31: "this owner-follower design we currently have gets the job done. but a
more practical design is distributed-file-system. that way, I don't need to have dk-server-2
running, in order to run drain." He then chose to keep owner-follower and add sync: "we can
use the git to perform sync. When each server starts up, they sync via the git repo". For the
outage case he proposed "1. simple solution: add task_id buffer … And when the downed server
comes back online, it will add the tasks in-between the buffer 2. better solution: servers are
synced on board meta data on every pull or poke calls."

A distributed filesystem is ruled out. The task store is SQLite, and its locking and WAL do not
work over network filesystems. Shared-file replication would also bring back the multi-writer
problems that the retired Task Sync proposal had to solve
([task-publication vision §2](../task-publication/3_vision.md#retired-task-sync)).

The current contracts rule out failover in three places:

- **Owner-only task state.** Distributed-drain
  [§1](../distributed-drain/2_design.md#1-roles-one-owner-n-followers) fixes one owner per
  repository, and [§5](../distributed-drain/2_design.md#5-transport-and-authority-routing)
  routes every task read and write to it, with "no replica-local fallback". A follower's local
  store does not hold the tasks it runs. The claimed leaf reads its task from the
  `claimed_task` snapshot in the admission receipt
  ([§3](../distributed-drain/2_design.md#3-pull-mode-drain-and-the-pulled-leaf-pipeline)).
- **One-way publication.** Task-publication
  [§1](../task-publication/2_design.md#1-ownership-boundary) lets only the declared owner
  publish. [§5](../task-publication/2_design.md#5-compare-and-swap-and-competing-writers) and
  [§8](../task-publication/2_design.md#8-failure-semantics) treat a moved branch as an
  authority conflict that is never merged or forced.
  [§6](../task-publication/2_design.md#disaster-recovery) restores only into the same owner
  machine and defaults to an empty store. [§9](../task-publication/2_design.md#9-concerns--honest-limitations)
  defers authority transfer and multi-writer sync.
- **Weekly backup.** Publication is explicit, and its routine is weekly and ships disabled
  ([task-publication §4](../task-publication/2_design.md#4-snapshot-construction)). A weekly
  snapshot cannot carry a takeover.

## 2. Core Concepts

- **Ownership epoch.** A counter that starts at 1 and rises by one with every handoff or
  takeover. Each epoch names exactly one owner machine and one task-ID range. A host acts as
  owner only while it holds the highest epoch it has seen.
- **Fence commit.** A commit on the workspace's publication branch that starts a new epoch.
  The branch's existing compare-and-swap push decides which competing fence commit wins.
- **Change log and cursor.** A new owner-side table with one row per committed task change. A
  row's `(epoch, seq)` is the cursor followers resume from. Only the owner writes it.
- **Mirror.** A follower's read-only copy of the owner's task records, kept in its own
  partition and labelled with its cursor and fetch time. It carries no authority while the
  owner is reachable.
- **Mirror head and poke.** Every distributed-drain response carries the owner's current
  `(epoch, seq)` head. A follower that is behind fetches the delta with a read-only
  `orbit.mirror.changes` call. The same call, run on a timer by an idle follower, is the poke.
- **Handoff and takeover.** A handoff is planned. The old owner freezes writes, publishes,
  and writes the fence commit itself, so no data is lost. A takeover is unplanned. An operator
  runs it on a follower while the owner is unreachable. It can miss the writes that reached
  the old owner after the follower's last mirror sync.
- **Deposed owner.** An old owner that has seen a higher epoch. It refuses task writes, admission
  and drains, and serves reads marked as stale until it rejoins.
- **Rejoin and conflict list.** The deposed owner sends the writes it accepted after the takeover
  base to the new owner. New tasks and append-only records merge. A task changed on both sides
  goes onto a conflict list for the operator, and is never resolved last-writer-wins.

## 3. At a Glance

All rows are proposed. The files named are the ones that would change. None of this is built.

| Concern | File | Task |
|---------|------|------|
| Change log in the task commit boundary | [crates/orbit-store/src/driver/sqlite/task_registry/](../../../crates/orbit-store/src/driver/sqlite/task_registry/) | [ORB-20620] |
| Epoch ID ranges on the allocator | [crates/orbit-store/src/driver/sqlite/task_registry/allocator.rs](../../../crates/orbit-store/src/driver/sqlite/task_registry/allocator.rs) | [ORB-20620] |
| Fence commits and on-change publication | [crates/orbit-store/src/workflow/task/publish.rs](../../../crates/orbit-store/src/workflow/task/publish.rs) | [ORB-20620] |
| Mirror, takeover and rejoin import | [crates/orbit-store/src/workflow/task/restore.rs](../../../crates/orbit-store/src/workflow/task/restore.rs) | [ORB-20620] |
| Owner/replica role flip | [crates/orbit-cmd/src/registry/runtime/selection.rs](../../../crates/orbit-cmd/src/registry/runtime/selection.rs) | [ORB-20620] |
| Data-loss windows and ID sizing | [2_design.md §5–§6](./2_design.md#5-taking-over-ownership) | [ORB-20620] |
| Open questions for Daniel | [2_design.md §10.1](./2_design.md#101-open-questions-for-daniel) | [ORB-20620] |

## Task References

- [ORB-20620] — wrote this proposal for epoch-fenced owner failover.
- [ORB-12488] — authored the distributed-drain owner/follower contract this design extends.
- [ORB-11068] — specified the dedicated publication repository this design reuses as the fence.
- [ORB-11077] — shipped the publication CLI and the disabled weekly routine.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
