---
title: Auto-concurrency — Overview
owner: claude
last_updated: 2026-10-13
last_validated: 2026-10-18
status: Draft
feature: auto-concurrency
doc_role: overview
type: design
summary: Partly implemented predictive admission — Orbit records per-task CPU and memory samples, builds resource profiles, forecasts host load, and supports shadow or forecast-based admission for local and pull drains; operational accuracy evidence remains limited.
tags: [auto-concurrency, drain, capacity, admission]
paths: ["crates/orbit-core/src/application/workspace_auto/classify.rs", "crates/orbit-core/src/application/pull_drain/refill/**", "crates/orbit-core/src/application/admission/**", "crates/orbit-core/src/runtime/host_resource/**", "crates/orbit-core/src/application/job/pipeline/worker/**", "crates/orbit-config/src/registry/settings/machine.rs", "crates/orbit-types/src/workflow/run/state/drain/mod.rs"]
related_features: [auto-concurrency, distributed-drain]
related_artifacts: [ORB-15757]
---

# Auto-concurrency — Overview

> **Status: Draft design; implementation is delivered, rollout evidence is
> still limited.** Linux and macOS worker sampling, host measurement,
> profiles, forecasting, admission reservations, local and pull-drain
> admission, diagnostics, and the starvation signal are in the current code
> ([2_design.md](./2_design.md) §2–§8). The fleet snapshot verified on
> 2026-10-13 was `shadow` with `admit_cpu_percent = 98`; this is deployment
> configuration, not a product default. The configuration defaults to
> `mode = off`, with CPU and memory thresholds of 90% and 75%.

The implementation supports forecast-based admission, but that does not
establish forecast accuracy or readiness to enforce it. The native Mac
operational report from 2026-10-13 at 22:57:50 UTC recorded one paired
forecast and measured host point;
that confirms the reporting path, not a useful accuracy cohort or a safe
threshold. Historical Mac worker minutes without corrected CPU-unit
provenance remain stored but are excluded from profiles, so the eligible Mac
cohort is newer still. Keep that historical fleet snapshot separate from the
delivered capability; it is not a current fleet-mode claim. See [ORB-15980]
and [ORB-15994] for the native measurement repairs and [ORB-15775] for
forecast recording and accuracy reporting.

The accuracy report now supports operator-selected forecast-time, drain and
recorded-mode cohorts, and separates unfinished target minutes, unrecordable
partial minutes, and mature sampling gaps. This is delivered reporting capability, with no admission or
configuration retune. A selected cohort does not attest source/build-budget
homogeneity, and does not turn sparse per-task profiles or wide spread into
calibrated confidence. See [§8.1](2_design.md#81-the-accuracy-report) and the
[forecast accuracy runbook](../../runbooks/forecast-accuracy.md).

A drain's concurrency today is a number the operator picks.
**Auto-concurrency** admits tasks by predicted host load instead, up to that
number:

1. Orbit records each task's CPU and memory from its Linux worker scope or,
   on macOS, its worker process tree. Samples are aggregated into minutes
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

Historical fleet snapshot (2026-10-12): each drain carried a fixed
`max_active_leaf_runs`, set at start and changed with `orbit run concurrency`.
The observed ceilings were 15 on dk-server-2, 5 on dk-server-1 and 4 on the
Mac. Operators had chosen and revised them after incidents. These are
historical host settings, not product defaults or recommendations. Ceilings
that are too low leave capacity idle while backlog waits; ceilings that are
too high slow work and risk exhausting host memory.

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
  read from its Linux worker scope or its macOS worker process tree.
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
  under. Configuration orders the memory threshold below the throttle's
  memory high mark. The throttle defaults to Linux load, a different signal
  from forecast CPU utilization; `workflow.resource_throttle.cpu_source = "utilization"` opts
  into busy tick accounting. CPU thresholds remain independently configured.
- **Backstop** — the existing resource throttle. It still holds all
  admission when real pressure arrives.

## 3. At a Glance

The rows below map the implemented capabilities to their delivery tasks and
current source. Later native repairs and the starvation repair are listed
alongside the original implementation work; source links follow the current
`application/` layout.

| Concern | Current source | Task |
|---------|---------------|------|
| Linux per-task sampling from the worker scope | [worker/usage.rs](../../../crates/orbit-core/src/application/job/pipeline/worker/usage.rs) | ORB-15768 |
| Per-task and host resource-minute storage | [resource_usage_store.rs](../../../crates/orbit-store/src/driver/sqlite/resource_usage_store.rs) | ORB-15768 |
| Resource profiles and `orbit run profiles` | [host_resource/profile/](../../../crates/orbit-core/src/runtime/host_resource/profile/mod.rs), [run/profiles.rs](../../../crates/orbit-cli/src/command/run/profiles.rs) | ORB-15770 |
| Analytics › Resources: profile charts in the dashboard | [api/resource_profiles.rs](../../../crates/orbit-web/src/api/resource_profiles.rs), [js/resources.js](../../../crates/orbit-web/assets/dashboard/js/resources.js) | ORB-16616 |
| Host-load forecast and admission reservations | [host_resource/forecast/](../../../crates/orbit-core/src/runtime/host_resource/forecast/mod.rs), [admission_load_store.rs](../../../crates/orbit-store/src/driver/sqlite/admission_load_store.rs) | ORB-15771 |
| Gap-fit admission in local drains | [workspace_auto/classify.rs](../../../crates/orbit-core/src/application/workspace_auto/classify.rs), [admission/gap_fit.rs](../../../crates/orbit-core/src/application/admission/gap_fit.rs) | ORB-15772 |
| Forecast admission for pull drains | [pull_drain/refill/](../../../crates/orbit-core/src/application/pull_drain/refill/mod.rs), [distributed/work_fit.rs](../../../crates/orbit-core/src/application/distributed/work_fit.rs) | ORB-15774 |
| Host-only `[machine.auto_concurrency]` settings | [settings/machine.rs](../../../crates/orbit-config/src/registry/settings/machine.rs) | ORB-15769 |
| Admission reasons in drain state, `orbit run show`, and readiness | [run/state/drain/mod.rs](../../../crates/orbit-types/src/workflow/run/state/drain/mod.rs), [run/admission_forecast.rs](../../../crates/orbit-cli/src/command/run/admission_forecast.rs) | ORB-15773 |
| Forecast recording and `orbit run profiles --accuracy` | [host_resource/forecast/record.rs](../../../crates/orbit-core/src/runtime/host_resource/forecast/record.rs), [host_resource/accuracy.rs](../../../crates/orbit-core/src/runtime/host_resource/accuracy.rs) | ORB-15775 |
| macOS worker sampling from the process tree | [worker/usage_tree.rs](../../../crates/orbit-core/src/application/job/pipeline/worker/usage_tree.rs), [process/ancestry.rs](../../../crates/orbit-common/src/process/ancestry.rs) | ORB-15776 |
| Native macOS worker CPU-unit calibration and provenance | [worker/usage_tree.rs](../../../crates/orbit-core/src/application/job/pipeline/worker/usage_tree.rs) | ORB-15994 |
| Native macOS host CPU and memory measurements | [host_resource/usage.rs](../../../crates/orbit-core/src/runtime/host_resource/usage.rs), [host_resource/platform.rs](../../../crates/orbit-core/src/runtime/host_resource/platform.rs) | ORB-15980 |
| Protected-head starvation signal and friction | [admission/gap_fit.rs](../../../crates/orbit-core/src/application/admission/gap_fit.rs), [admission/stuck_head.rs](../../../crates/orbit-core/src/application/admission/stuck_head.rs) | ORB-15894 |
| Threshold-one retry deduplication repair | [admission/stuck_head.rs](../../../crates/orbit-core/src/application/admission/stuck_head.rs), [workspace_auto/classify.rs](../../../crates/orbit-core/src/application/workspace_auto/classify.rs) | ORB-16014 |

## Task References

- [ORB-15757] — authored the original proposal.
- ORB-15768–ORB-15776 — delivered the original implementation steps; see the
  table for each task's source.
- ORB-15980 and ORB-15994 — completed native macOS measurement repairs.
- ORB-15894 and ORB-16014 — delivered and repaired the protected-head
  starvation signal.

The links above point to the source in the current tree. Task records carry
the delivery and native operational evidence; the Mac pair reported on 2026-10-13
does not demonstrate forecast accuracy or enforcement readiness.

## Per-drain control

Local drains inherit the configured host mode on each admission pass, or use an
explicit `orbit run auto --auto-concurrency off|shadow|enforce` override.
Dashboard operators and registered MCP/HTTP starts use the same core contract.
Explicit overrides survive resume and restart without editing configuration.
Local and pull drains reload host thresholds and effective provider-limit
policy on the next pass, caching unchanged configuration by mtime; invalid
configuration warns and retains the previous policies. Pull drains inherit
executor host mode and reject explicit per-drain overrides. See [surfaces and configuration](2_design.md#7-surfaces-and-configuration).
