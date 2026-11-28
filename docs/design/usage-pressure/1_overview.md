---
title: Usage Pressure — Overview
owner: claude
created_date: 2026-10-23
updated_date: 2026-11-28
last_validated: 2026-11-20
status: Accepted
stage: shipped
next: "—"
feature: usage-pressure
doc_role: overview
type: design
summary: "Weight crew pool draws by how much of each provider account's usage window would expire unused, so Orbit spends the capacity that is about to reset before the capacity that is not"
tags: [usage-pressure, crews, crew-pools, provider-limits, admission]
---

# Usage Pressure — Overview

Usage pressure tilts crew pool draws toward accounts whose subscription capacity expires first, using the provider usage readings Orbit already records, so less of the combined capacity is lost at resets. It only scales the configured weights; which crews a source may draw does not change.

Status: phases 1–4 (pressure view, task draws, shared pool draws and pull drains) ([`workflow.crew_selection`](../../config/crews.md#usage-pressure-draws)) implemented.
The phase-1 task [ORB-17769] records Daniel's approval of this scope on 2026-10-23;
all four implementation phases are done, so its document status is Accepted and its stage is shipped.

## 1. Motivation

Bearing: [provider usage limits](../../config/crews.md#provider-usage-limits) already record every
window a provider reports ([ORB-14695] failures, [ORB-14696] post-run readings, [ORB-17644] Muse)
and gate crews that reach a threshold ([ORB-14697]). Weighted complexity and purpose pools
(`workflow.<complexity>_complexity_crews`, `workflow.purpose_crews.*`) choose among the crews
that are left. Task-type draws also use a `type:<type>/<tier>` source. This scope uses the
readings to tilt those draws too, without changing which crews a source may draw.

### The gap

Subscription capacity disappears at its reset. On a Friday evening the Claude login on a host
reads `seven_day` 10% used, resetting in 6 hours. The Codex login reads its weekly window 0%
used, resetting in 7 days. Ninety percent of a Claude week is lost at midnight unless Orbit
spends it, and every Codex token spent tonight is still available next week. Orbit has both
readings and treats the two accounts the same: the hard pool `opus:50, sonnet:25, sol:25` sends
a quarter of the hard tasks to Codex, and Claude crews only lose share when the gate trips.

Before this feature, the readings only removed crews; they did not prefer one.

A second fact shaped the fix. A task's pool crew was drawn **once, when the task was created**
(`CreationCrewAssignment`, source `pool:<complexity>` or `purpose:<purpose>`, in
[`candidate_assignment.rs`](../../../crates/orbit-core/src/application/job/crew_pools/candidate_assignment.rs)).
Admission then kept that member "while it can run", in `unheld_task_crew_candidates`, and only
re-drew when a hold or a limit removed it. A task filed on Tuesday could run on Friday with
Tuesday's draw. Weighting the creation draw alone would weight it with days-old readings.

### Intended outcome

Spend the capacity that expires first, so that less of the accounts' combined capacity is lost
at resets. Quality routing is unchanged: the task's source pool (complexity, purpose, or task
type) still decides which crews it may run as, and the configured weights still express the
operator's preference among them.
Pressure only scales those weights.

### Out of scope

- **Changing pool membership or task routing.** Pressure never sends a task to a crew outside
  its existing complexity, purpose, or task-type pool.
- **Owner-local admission order.** Pressure does not change which ready task an owner admits
  next locally. Pull offer order is the one exception (phase 4): a follower's reported pressure
  is the lowest ordering key for its offers, below band, priority, expiry and census affinity.
  See [2_design.md §Open questions](./2_design.md#open-questions).
- **Concurrency.** Admitting more work while pressure is high belongs to
  [auto-concurrency](../auto-concurrency/1_overview.md), not here.
- **Absolute capacity.** No conversion of percentages into tokens across providers.

### Acceptance (feature level)

- With `crew_selection = "usage_pressure"`, a hard pool `opus:50, sonnet:25, sol:25`, Claude
  `seven_day` at 10% resetting in 6 h and Codex weekly at 0% resetting in 7 d, the admission
  draw's `eligible_pool` shows Claude members at ×8 and `sol` at ×1, and `orbit run show` names
  usage pressure in the source.
- A task created with `crew = sol, crew_source = pool:hard` before those readings is re-drawn at
  admission, and its record and history show the new crew. A task with an explicit `sol` crew is
  not re-drawn.
- With Claude `seven_day` 95% used and resetting in 6 days, Claude members draw at ×1/8 and are
  not gated until a window crosses the threshold.
- A crew with no reading, and every crew under `crew_selection = "static"`, draws on its
  configured weight exactly as before. Seeded draws reproduce byte-for-byte.
- Readiness and doctor show each account's pressure and the window that set it.

The mechanisms are in [2_design.md](./2_design.md); decisions that qualify are kept in [4_decisions.md](./4_decisions.md).

## 2. Core Concepts

- **Pressure.** How much of a usage window's capacity remains against how soon it resets; high pressure means capacity that will expire unspent.
- **Crew pressure.** The pressure of a crew's account, taken from the window that sets it and shown in readiness and doctor.
- **Effective weight.** A pool member's configured weight scaled by its pressure; a crew with no reading, or any crew under `crew_selection = "static"`, keeps its configured weight.
- **Draw point.** Where pressure applies: the creation draw, the re-draw at admission, shared pool draws and a pull follower's offer order.
- **Source pool.** The complexity, purpose or task-type pool that still decides which crews a task may run as.

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| Usage readings pressure is computed from | [crews config](../../config/crews.md#provider-usage-limits) | [ORB-14695], [ORB-14696], [ORB-17644] |
| Crews gated at their limit | [crews config](../../config/crews.md#provider-usage-limits) | [ORB-14697] |
| Pressure of one window and of a crew | [2_design.md](./2_design.md#pressure-of-one-window) | [ORB-17769] |
| Effective weight | [2_design.md](./2_design.md#effective-weight) | [ORB-17769] |
| Where pressure applies | [2_design.md](./2_design.md#where-pressure-applies) | [ORB-17769] |
| Creation draw | [candidate_assignment.rs](../../../crates/orbit-core/src/application/job/crew_pools/candidate_assignment.rs) | [ORB-17769] |
| Standing rules | [4_decisions.md](./4_decisions.md) | — |

## Task References

- [ORB-14695] — classifies usage-limit failures and records them in the per-host limit store
- [ORB-14696] — records each provider's reported usage windows after every run
- [ORB-14697] — gates crews at or near their usage limit out of pool draws and admission
- [ORB-17644] — reads Muse usage windows into provider-limit readings
- [ORB-17769] — computes and displays weekly usage pressure

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
