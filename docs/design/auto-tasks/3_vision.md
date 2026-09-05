---
title: Auto-tasks — Vision
owner: claude
last_updated: 2026-09-05
last_validated: 2026-09-05
status: Accepted
feature: auto-tasks
doc_role: vision
type: design
summary: Forward-looking directions for the auto-task primitive — cross-workspace scope, richer templates, and dispatch coupling.
tags: [auto-tasks]
paths: ["crates/orbit-core/src/application/auto_tasks/**"]
related_features: [auto-tasks]
related_artifacts: [ORB-10149, ORB-11315]
---

# Auto-tasks — Vision

Forward-looking directions for the primitive. Everything here is speculative and
deliberately unbuilt; the shipped surface is in 2_design.md.

The [shared automation-trigger proposal](../automation-triggers/1_overview.md)
from [ORB-11315] specifies delivery thresholds, preparation/failure eligibility,
immutable batches and separate successful-coverage checkpoints. It is proposed
and unimplemented; existing scheduling and action semantics remain current.

## 1. Open Questions

1. **Cross-workspace scheduling.** Today the scheduler processes only the
   definitions of the workspace whose routine fired it. Should there be one
   host-level pass that fans out over every routine-source workspace's
   `auto_tasks/`, mirroring the routine sweep's discovery?
2. **Dispatch coupling.** A minted task lands in `backlog`; the orchestrator
   still triages/ships it. Should a definition optionally auto-dispatch its
   task (e.g. straight into `workflow_ship`) under a crew, or does that
   re-introduce the "periodic work is code" coupling auto-tasks removed?
3. **Retention / expiry.** Should a definition support a `max_open` or a
   sunset date so one-off recurring campaigns retire themselves?
4. **Observability depth.** Routine fires are visible on `/api/routines`, but
   per-definition history (which slots minted which tasks) currently lives only
   in the cursor's `last_task_id`. Is a fuller per-definition ledger warranted?

## 2. Prior Work

### Within orbit
- **Routines** (`docs/design/routines/`) — the scheduler machinery auto-tasks
  ride on (cron eval, fire records, host pinning, dashboard health).
- **qa-sweep** (ORB-10039) — a bespoke periodic sweep that auto-tasks generalize;
  qa-sweep V1 (ORB-10148) is the first auto-task definition.
- **Triage pipeline** (ORB-10129) — the closest existing "routine fires a job of
  deterministic steps" shape.

### External
- Cron / systemd timers — the "schedule + command" baseline; auto-tasks add
  catch-up collapse, dedupe, and a task-shaped payload.
- Temporal/Cadence schedules — durable, catch-up-aware recurring workflows; the
  collapse semantics here echo their "skip overlapping" backfill policy.

## 3. What May Be Distinctive

The payload is a **task**, not a command. A fire produces a first-class Orbit
task that flows through the normal lifecycle (triage, crew routing, review), so
the scheduler needs no privileged execution surface — the dedupe key is just the
task's provenance tag, and observability is the existing task + routine surfaces.

## 4. References

### Orbit-internal
- `docs/design/routines/` — scheduler substrate.
- [Auto-task primitive: file-backed recurring task templates + one generic scheduler routine](./4_decisions.md#auto-task-primitive-file-backed-recurring-task-templates-one-generic-scheduler-routine) — the auto-task primitive decision.
- [Run budgets are provider-neutral: wall-clock timeouts, never turn caps](./4_decisions.md#run-budgets-are-provider-neutral-wall-clock-timeouts-never-turn-caps) — provider-neutral run budgets (no turn caps).

### External
- POSIX cron; systemd timer `Persistent=` (catch-up analogue).

## Task References

- [ORB-11315] — proposes shared state-driven triggers and durable coverage semantics.

- ORB-10149 — Auto-task primitive.
- ORB-10148 — qa-sweep V1.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
