---
title: Resident Orchestrator — Vision
owner: codex, grok, claude
last_updated: 2026-08-15
last_validated: 2026-09-10
status: Draft
feature: resident-orchestrator
doc_role: vision
type: design
summary: After the epic-owned worktree and the drain window exist, later work may add a conflict tolerance threshold, a forced ship override, an Orbit-owned clock, or conversation resume.
tags: [resident-orchestrator, epic, jobs]
paths: [".orbit/resources/jobs/**"]
related_features: [resident-orchestrator, routines]
related_artifacts: [ORB-10775, ORB-10788, ORB-10815]
---

# Resident Orchestrator — Vision

V1 proved that a scan-and-drain job plus an external clock is enough. [ORB-10815] moves the epic
into its own worktree and turns the auto tick into a window. The items below stay out of both.

## 1. Open Questions

1. **How many context-file conflicts should a leaf tolerate?** Admission is all-or-nothing today:
   any overlap with a live holder excludes the task. A threshold ("N conflicting files are
   acceptable") is the natural generalization — `task_overlap_conflicts` already returns the
   conflict list rather than a boolean, so the policy has somewhere to live. Not in [ORB-10815].
2. **Should `orbit run ship --force` exist?** An explicit override of the context-lock gate for a
   named task. The gate it would bypass is `task_gate_pipeline`'s `reserve_locks` wait loop, not
   `list_backlog`'s discovery filter — explicit ids already skip the latter. Force should reserve
   *despite* conflicts rather than skip reservation, so the forced task stays a visible holder, and
   it moves the failure to merge time, after an agent has already done the work. Parked
   deliberately.
3. **Should Orbit grow a routine that fires `epic_pipeline` or `workspace_auto_pipeline`?** Only
   after the jobs are boring in production. A seeded routine is how this becomes a resident server
   by accident. Retargeting `workspace_ship_pipeline` is not a new routine.
4. **Should `scan_unresolved_work` itself become epic-scoped?** [ORB-10818] gives `epic_pipeline` a
   separate epic-scoped completion gate rather than teaching the workspace scan a filter. If a
   workspace routinely holds several independent bodies of work, revisit — but prefer separate
   workspaces first.
5. **Should an epic's children ever run in parallel?** Sequential is what makes the single
   fast-forward lane work. Parallel children would need a real merge strategy on the epic branch
   and conflict handling the finisher currently gets for free. Not worth it until serial drain is
   demonstrably the bottleneck.
6. **Conversation resume?** Session log is the memory. Resume is still allowed later as a fail-open
   optimization, never as the notebook.
7. **Event-driven wake?** A task-created or run-failed hook would cut latency further than a poll
   loop. Routines have no event trigger; do not add one for this feature.
8. **Multiple orchestrator identities per workspace?** Requires a routing key. Tags alone are not
   enough.

## 2. Prior Work

The first draft of this folder specified an in-Orbit resident (CLI session, decision comments,
`select_resident_epic`, seeded routine). Polaris `design/orchestrator/reconciler.md` already argued
the opposite: a stateless tick over Orbit + runs, no second store. V1 took the tick and left the
clock outside. V2 keeps the clock outside and makes the tick a window.

## 3. What May Be Distinctive

The drain predicate stays boring on purpose: task statuses and run states, not model judgment. What
changed is where the work happens. An epic is a place — one worktree, one branch, one review — and
the agent that finishes it lives there. The job, not the model, still decides whether work remains.

## 4. References

- [Design](./2_design.md)
- Polaris `design/orchestrator/reconciler.md` (constellation tree)

## Task References

- **[ORB-10775]** — v1 drain epic.
- **[ORB-10788]** — v1 sequencer.
- **[ORB-10815]** — Epic-owned worktree and drain window. Items in §1 stay out.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
