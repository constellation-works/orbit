---
title: Auto-concurrency — Overview
owner: claude
last_updated: 2026-10-11
last_validated: 2026-10-11
status: Draft
feature: auto-concurrency
doc_role: overview
type: design
summary: Proposal for drains that set their own concurrency — a per-host leaf budget moved between operator bounds by a congestion-avoidance loop and rated by landed goodput, seeded from an estimate over run history.
tags: [auto-concurrency, drain, capacity, admission]
paths: ["crates/orbit-core/src/adapter/engine_host/v2_host/workspace_auto/classify.rs", "crates/orbit-core/src/adapter/engine_host/v2_host/pull/refill.rs", "crates/orbit-core/src/application/job/run/worker_limit.rs", "crates/orbit-store/src/driver/sqlite/job_run_store/pull.rs", "crates/orbit-core/src/runtime/host_resource/**"]
related_features: [auto-concurrency, distributed-drain, automation-triggers, routines]
related_artifacts: [ORB-15757]
---

# Auto-concurrency — Overview

> **Status: Draft proposal, not implemented.** Nothing in this folder exists
> in code yet. It records the design for review before implementation tasks
> are filed.

A drain's concurrency is a number the operator picks: how many leaves may run
at once. Too low leaves capacity idle while backlog waits. Too high slows
every leaf, raises conflicts, provider limits and failures, and can tip a host
over. The right number moves with the binary, the task mix, provider state
and the other drains on the host. **Auto-concurrency** lets Orbit set it. Each
host has one leaf budget, shared by every drain on it. A congestion-avoidance
loop moves the budget between the operator's minimum and maximum: it cuts
quickly when the host shows strain and grows slowly when demand fills every
slot. A slower loop rates each level by goodput, the landed work that stayed
landed. An estimate from run history seeds that rating before the controller
ever acts.

## 1. Motivation

Today each drain carries a fixed `max_active_leaf_runs`, set at start and
changed by hand with `orbit run concurrency`. At the time of writing, the
operator runs 15 on dk-server-2, 5 on dk-server-1 and 4 on the Mac. These
numbers were chosen by feel and revised after incidents such as a load of 85
on 15 cores and a full disk. Nothing in Orbit measures whether they are
right. The resource throttle holds admission when CPU, memory or disk
cross a high mark, but it is an on/off gate at the edge of overload. It does
not find the level where throughput peaks, and it never lowers a ceiling.
No host-wide limit exists either: two workspaces draining on one host each
take their full ceiling.

The history needed to do better largely exists. Every leaf run has a host,
start and end times, and an outcome. Every task has a landing record. The
question "would one more slot have helped?" is answerable from that data, at
least roughly. Answering it continuously is a control problem with
well-known solutions ([3_vision.md §2](./3_vision.md#2-prior-work)).

## 2. Core Concepts

- **Goodput** — baseline work-hours of leaves that landed and stayed landed,
  per wall-clock hour. The objective; completions are never optimized
  ([2_design.md §1](./2_design.md#1-the-objective-is-goodput)).
- **Slowdown** — a leaf's duration divided by the baseline for its
  complexity class in its workspace. It removes the task mix from the
  measurement.
- **Occupancy** — the number of live leaf runs on a host, counted across all
  workspaces.
- **Saturated interval** — an interval in which every slot was busy and
  admissible work was still waiting. Only these say anything about capacity.
- **Host budget** — the one number the controller tunes: the live leaves a
  host may run across every drain, between the operator's bounds
  ([2_design.md §3.2](./2_design.md#32-proposed-a-host-budget-and-workspace-caps)).
- **Workspace cap** — a bound on one workspace's leaves across hosts, set by
  contention on its base branch rather than by host resources.
- **Congestion signal** — fast evidence of strain: a resource-throttle hold,
  a provider-limit exclusion, an environmental failure or validation-step
  latency inflation. Congestion signals only ever cause a decrease
  ([2_design.md §4](./2_design.md#4-signals)).
- **Soft ceiling** — the highest level the goodput model supports, below the
  operator's maximum, re-tested daily.
- **Pin** — a manual resize. It takes the drain out of automatic control for
  the rest of its window.
- **Shadow mode** — the controller decides and records but does not act.

## 3. At a Glance

Implementation tasks are filed from this table once the design is accepted.

| Concern | File | Task |
|---------|------|------|
| Goodput, slowdown and baseline definitions | [2_design.md §1](./2_design.md#1-the-objective-is-goodput) | [ORB-15757] |
| Offline estimator and `orbit run concurrency --report` | new, beside [worker_limit.rs](../../../crates/orbit-core/src/application/job/run/worker_limit.rs) | to file |
| Host occupancy, host budget and its history table | [job_run_store/pull.rs](../../../crates/orbit-store/src/driver/sqlite/job_run_store/pull.rs) | to file |
| Budget split and admission check (local) | [workspace_auto/classify.rs](../../../crates/orbit-core/src/adapter/engine_host/v2_host/workspace_auto/classify.rs) | to file |
| Budget check (pull) | [pull/refill.rs](../../../crates/orbit-core/src/adapter/engine_host/v2_host/pull/refill.rs) | to file |
| Congestion signals | [runtime/host_resource/](../../../crates/orbit-core/src/runtime/host_resource/mod.rs), [settlement/runtime.rs](../../../crates/orbit-core/src/application/distributed/settlement/runtime.rs) | to file |
| Controller and actuation through the worker limit | [worker_limit.rs](../../../crates/orbit-core/src/application/job/run/worker_limit.rs) | to file |
| `[workflow.auto_concurrency]` settings | [settings/machine.rs](../../../crates/orbit-config/src/registry/settings/machine.rs) | to file |
| CLI: `--concurrency auto`, `--auto`, run show and readiness | [run/concurrency.rs](../../../crates/orbit-cli/src/command/run/concurrency.rs), [run/auto.rs](../../../crates/orbit-cli/src/command/run/auto.rs) | to file |
| Workspace cap at the owner's pull | [job_run_store/pull.rs](../../../crates/orbit-store/src/driver/sqlite/job_run_store/pull.rs) | to file |

## Task References

- [ORB-15757] — authored this proposal.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
