---
title: Auto-concurrency — Overview
owner: claude
last_updated: 2026-10-12
last_validated: 2026-10-12
status: Draft
feature: auto-concurrency
doc_role: overview
type: design
summary: Proposal for predictive admission — Orbit records each task's CPU and memory minute by minute, averages them into profiles per workspace, work class (code change, no-diff review, recurring auto-task) and complexity, forecasts host load from the running tasks, and admits a task when its profile fits under the forecast.
tags: [auto-concurrency, drain, capacity, admission]
paths: ["crates/orbit-core/src/adapter/engine_host/v2_host/workspace_auto/classify.rs", "crates/orbit-core/src/adapter/engine_host/v2_host/pull/refill.rs", "crates/orbit-core/src/runtime/host_resource/**", "crates/orbit-core/src/application/job/pipeline/worker/scope.rs"]
related_features: [auto-concurrency, distributed-drain]
related_artifacts: [ORB-15757]
---

# Auto-concurrency — Overview

> **Status: Draft proposal, partly implemented.** Recording, profiles, the
> load forecast, the host admission lock, gap-fit admission in local drains
> and forecast admission in pull drains exist in code
> ([2_design.md](./2_design.md) §2–§6);
> `machine.auto_concurrency.mode` defaults to `off`.

A drain's concurrency today is a number the operator picks.
**Auto-concurrency** admits tasks by predicted host load instead, up to that
number:

1. Orbit records how much CPU and memory each task uses, minute by minute,
   from admission to completion.
2. It averages those records into **resource profiles** per workspace,
   **work class** and complexity. Work class distinguishes code changes,
   which build and test, from no-diff reviews, which don't, and from
   recurring auto-task templates.
3. Before admitting a task, the drain **forecasts** the host's load over the
   coming minutes. The forecast is the profiles of the tasks already running,
   each advanced to its current age, plus the load that isn't from tasks.
4. A candidate is admitted when its own profile fits under the forecast,
   within the thresholds.

**Example.** An easy task usually spikes five minutes after it starts and
stays busy for five minutes. If the forecast shows headroom from five to ten
minutes out, the task is admitted, even while the host is busy now. If a
running task is about to start its build and the gap closes, the next
candidate waits.

## 1. Motivation

Each drain carries a fixed `max_active_leaf_runs`, set at start and changed
with `orbit run concurrency`. At the time of writing, the operator runs 15 on
dk-server-2, 5 on dk-server-1 and 4 on the Mac. The numbers were chosen by
feel and revised after incidents. When they are too low, capacity sits idle
while backlog waits. When they are too high, every task slows down, and a
host can run out of memory.

Orbit already has a reactive guard: the resource throttle holds admission
once CPU, memory or disk stays above 90%. That reacts to load that has
already arrived. A task's load arrives late: an agent spends its first
minutes reading and planning, and its builds and validation come later. So a
reactive check admits tasks while they look light and finds out when their
builds collide. Nothing limits load across workspaces either: two workspaces
draining on one host each take their full ceiling.

A task's resource use over time is regular enough to learn: the same kinds of
work in the same repository build at similar points. Knowing it lets
admission look ahead.

## 2. Core Concepts

- **Maximum concurrency** — the drain's existing `max_active_leaf_runs`.
  It stays the operator's hard ceiling.
- **Resource sample** — one task's CPU and memory for one minute of its life,
  read from its own Linux worker scope.
- **Work class** — what kind of work a task does: `change`, `no-diff`
  (tagged `no-diff-expected`, which must never compile or run tests), or one
  recurring `auto-task:` template.
- **Resource profile** — expected CPU and memory at each minute since
  admission, for one workspace, work class and complexity on one host, with
  the chance that a task is still running at that minute.
- **Load forecast** — the host's expected CPU and memory for each minute of
  the horizon: running tasks' profiles plus the load that isn't from tasks.
- **Gap fit** — admitting a candidate because the forecast plus its profile
  stays under the admit thresholds for its whole horizon.
- **Admit thresholds** — the CPU and memory levels a forecast must stay
  under. They sit below the throttle's high marks.
- **Backstop** — the existing resource throttle. It still holds all
  admission when real pressure arrives.

## 3. At a Glance

Implementation tasks are filed from this table once the design is accepted.

| Concern | File | Task |
|---------|------|------|
| Per-task resource sampling from the worker's own scope | [worker/scope.rs](../../../crates/orbit-core/src/application/job/pipeline/worker/scope.rs) | to file |
| Resource sample storage | [job_run_store/](../../../crates/orbit-store/src/driver/sqlite/job_run_store/pull.rs) | to file |
| Profiles and `orbit run profiles` report | [host_resource/profile/](../../../crates/orbit-core/src/runtime/host_resource/profile/mod.rs) | to file |
| Host load forecast and admission reservations | new, beside [host_resource/](../../../crates/orbit-core/src/runtime/host_resource/mod.rs) | to file |
| Gap-fit admission (local drains) | [workspace_auto/classify.rs](../../../crates/orbit-core/src/adapter/engine_host/v2_host/workspace_auto/classify.rs) | to file |
| Gap-fit admission (pull drains) | [pull/refill.rs](../../../crates/orbit-core/src/adapter/engine_host/v2_host/pull/refill.rs) | to file |
| `[workflow.auto_concurrency]` settings | [settings/machine.rs](../../../crates/orbit-config/src/registry/settings/machine.rs) | to file |
| Admission reasons in `drain_last_pass` and `orbit run show` | [run/state/drain.rs](../../../crates/orbit-types/src/workflow/run/state/drain.rs) | to file |
| macOS sampling from the worker's process tree | [process/ancestry.rs](../../../crates/orbit-common/src/process/ancestry.rs) | to file |

## Task References

- [ORB-15757] — authored this proposal.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
