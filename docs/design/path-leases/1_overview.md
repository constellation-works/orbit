---
title: Path Leases — Overview
owner: claude
last_updated: 2026-10-25
last_validated: 2026-10-25
status: Draft
feature: path-leases
doc_role: overview
type: design
summary: "Proposed: let several people and their agents, outside Orbit's drains, lease repository paths on one owner, so their work and drain tasks share one lock index, wait for a green light when needed, and hear when overlapping work lands; push and poll over MCP, CLI, REST and SSE, with lease tokens for everyone but the operator."
tags: [path-leases, locks, admission, reservations, notifications]
paths: ["crates/orbit-runtime/src/task/locks/**", "crates/orbit-store/src/driver/sqlite/task_reservation_store/**", "crates/orbit-web/src/api/**"]
related_features: [conflict-budget, distributed-drain, remote-access, federated-mcp]
related_artifacts: []
---

# Path Leases — Overview

> **Status: Draft design; nothing is implemented.** The proposal builds on task reservations,
> which exist today ([2_design.md §1](./2_design.md#1-what-exists-today)).

Orbit already keeps parallel drain tasks from editing the same files: an in-progress or review
task holds a lock on its declared footprint, and admission defers a candidate that overlaps it.
Work that does not run through a drain, such as a Claude Code or Codex session, an editor, or a
person at the keyboard, is invisible to that lock. A **path lease** lets any such client ask the
owner "I'm about to work on these paths", get a green light now or a place in line, and be told
when work that overlaps it lands. The engine is the owner's existing lock index; what is new is a
user-level surface (push a request, then poll or stream) instead of host-level drain pull, and
a token-authenticated listener so that several people and their agents can coordinate on one
repository through one owner.

## 1. Motivation

Measured on ws_orbit on 2026-10-25 (read-only; the queries are described in
[2_design.md §10](./2_design.md#10-concerns--honest-limitations)):

- **Conflicts are rare but not free.** Since 2026-10-06, 110 runs needed a
  `pr_conflict_recovery` agent against 2,364 landed tasks (about 4.7%), and in 90 days 24 tasks
  blocked on a base conflict recovery could not fix (about 0.7% of 3,448 landed).
- **Declarations are incomplete.** Pilot footprints cover 59–69% of the files the landed commit
  changed, at 82–87% precision. About 40% of the recovered conflicts trace to a file one side
  never declared.
- **Exclusive locks over-block.** Of 326 lock denials where both tasks have since landed, 100
  (31%) never changed a file in common.
- **Outside work is unaccounted for.** Interactive sessions on the same repository take no lock
  at all, so neither they nor the drain can see each other until a rebase fails.
- **Only one person can reach the lock.** The owner is reached over SSH, and an SSH login is
  ownership of it. A second person working on the same repository, or their agents, has no way
  to see or take a lock without being handed the whole owner.

So the target is best-effort conflict reduction, not a guarantee: tell people and agents about
overlap early, let exclusive holds exist where a path policy wants them, and keep git, rebase and
`pr_conflict_recovery` as the backstop for what slips through.

## 2. Core Concepts

- **Path lease** — a reservation of path selectors on the owner, held by a principal rather than
  a task, with a mode, a priority, a lifetime and optionally a bound branch.
- **Mode** — `advisory` (recorded and announced, never blocks anyone) or `hard` (exclusive, like
  a task's context lock). Advisory is the default.
- **Principal** — who holds a lease: a person or a named agent. Over SSH it is attribution only;
  over the lease listener it is the authenticated token's subject.
- **Lease queue** — hard requests that overlap a hard holder wait in priority-then-age order
  instead of being refused.
- **Notice** — a durable event on a lease (granted, queued, expiring, preempted, released,
  overlap landed), read by polling a cursor or streaming the same cursor over SSE.
- **Landing notice** — the notice that work which changed paths an active lease holds has just
  landed on its base, so the holder should rebase now.
- **Lease token** — a bearer credential that authorizes lease operations, and nothing else, on
  the lease listener. Every principal other than the operator uses one: each person, and each of
  their agents.

Full definitions: [references/glossary.md](./references/glossary.md).

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| Reserve and release path selectors with a TTL (exists) | [crates/orbit-runtime/src/task/locks/commands.rs](../../../crates/orbit-runtime/src/task/locks/commands.rs) | — |
| Which tasks hold their context (exists) | [crates/orbit-runtime/src/task/locks/index.rs](../../../crates/orbit-runtime/src/task/locks/index.rs) | — |
| Reservation rows (exists; gains lease columns) | [crates/orbit-store/src/driver/sqlite/task_reservation_store/](../../../crates/orbit-store/src/driver/sqlite/task_reservation_store/) | — |
| Landed paths per task (exists; feeds landing notices) | [crates/orbit-core/src/application/codebase/facts/scan.rs](../../../crates/orbit-core/src/application/codebase/facts/scan.rs) | — |
| SSE streaming pattern (exists, for logs) | [crates/orbit-web/src/api/log.rs](../../../crates/orbit-web/src/api/log.rs) | — |
| Lease modes, queue, lifetime, notices | proposed, [2_design.md §2–§5](./2_design.md#2-the-lease-record) | — |
| MCP, CLI, REST and SSE surfaces | proposed, [2_design.md §6](./2_design.md#6-surfaces) | — |
| Principals, lease tokens and the lease listener | proposed, [2_design.md §7](./2_design.md#7-authority-and-authentication) | — |

Decisions: [4_decisions.md](./4_decisions.md). Open questions and prior work:
[3_vision.md](./3_vision.md).

## Task References

None yet; the design is unfiled.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
