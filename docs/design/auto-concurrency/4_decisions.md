---
title: Auto-concurrency — Decisions
owner: claude
last_updated: 2026-10-12
last_validated: 2026-10-12
status: Draft
feature: auto-concurrency
doc_role: decisions
type: design
summary: Proposed standing rules for predictive admission — measure what each task uses, forecast host-wide, keep the throttle as the backstop, assume unknown work is expensive, never exceed the operator's ceiling or cancel work.
tags: [auto-concurrency, drain, capacity, decisions]
paths: ["crates/orbit-core/src/application/admission/auto_admission.rs", "crates/orbit-core/src/runtime/host_resource/**"]
related_features: [auto-concurrency, distributed-drain]
related_artifacts: [ORB-15757]
---

# Auto-concurrency — Decisions

Record non-obvious decisions here by title. Task references carry provenance; superseded decisions remain in place so their original reasoning stays legible. See [CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule and required `Cost:` line.

Every entry is a proposed Door 2 rule: it governs choices the implementation
tasks make. Each is open to the operator's revision while the folder is
`Draft`. Per-task measurement, the host-wide forecast with its serialized
reservations, gap-fit admission in local drains under `shadow` and
`enforce`, and forecast admission in pull drains are implemented.

## Admit on forecast resource use, not on a throughput model

**Recorded:** 2026-10 · [ORB-15757]

### Context

The first draft of this design rated concurrency levels by landed work per
hour, fitted over hours of history, and moved a per-host limit with a
feedback loop. Signals that slow lag the host by hours. The operator chose
instead to admit by what tasks are about to use, learned from how earlier
tasks used CPU and memory over their lifetime.

### Decision

Admission decisions rest on measured or forecast resource use within the
next task's horizon. A model of throughput over hours may evaluate the
feature ([3_vision.md §1](./3_vision.md#1-open-questions), question 6), but
never drives admission.

### Consequences

- Admission reacts within one pass to a new task, a finished one, or a change
  in load.
- Cost: bottlenecks that aren't CPU or memory are invisible to it. When
  conflicts, review or provider quota limit throughput, admission still
  fills the host up to the operator's maximum.

## Measure what each task uses, not what the host uses

**Recorded:** 2026-10 · [ORB-15757]

### Context

Profiles need each task's own CPU and memory. The alternative is to infer
them from host totals, by regressing host load on the mix of tasks running.
That needs no new sampling, but it confounds every task with every other one
and with load that isn't from tasks.

### Decision

Resource profiles are built only from per-task measurements: the worker's
own Linux scope, or its process tree where no scope exists. Host totals are
used only for the baseline and to check the forecast.

### Consequences

- Attribution is exact on Linux, and the baseline separates out load that
  isn't from tasks.
- Cost: a host without per-task measurement has no profiles. That covers
  macOS until its process-tree sampler exists, and Linux with containment
  off. Such a host admits as today.

## Forecast and admit host-wide, one decision at a time

**Recorded:** 2026-10 · [ORB-15757]

### Context

Every drain on a host shares the same CPU and memory. Separate forecasts per
drain would each see the same gap and fill it twice.

### Decision

The forecast counts every live task on the host, across workspaces.
Admission decisions on a host are serialized under one lock, and decided but
not yet running admissions count as reservations. Any future admission rule
that uses host resources follows the same pattern.

### Consequences

- A gap is filled once, whichever drain gets there first.
- Cost: drains on one host queue behind each other's decisions, and a
  workspace with a deep backlog can take every gap before another workspace's
  pass runs.

## The resource throttle stays the backstop

**Recorded:** 2026-10 · [ORB-15757]

### Context

Gap fit admits tasks into a host that is busy now, because the forecast says
their load comes later. Forecasts are wrong some of the time.

### Decision

Forecast admission only ever removes candidates. It never overrides the
resource throttle, whose hold stops all admission when real pressure
arrives. Admit thresholds stay below the throttle's marks. No later
admission mechanism replaces the throttle; it can only be tightened.

### Consequences

- A bad forecast costs at most the tasks admitted before real pressure
  appears.
- Cost: two sets of thresholds to configure. The throttle can block an
  admission the forecast approved. Overload from a bad forecast lasts until
  enough tasks finish, because nothing is cancelled.

## Unknown work is assumed expensive

**Recorded:** 2026-10 · [ORB-15757]

### Context

A task whose workspace, work class or complexity has too few deliveries has
no profile of its own. Treating it as free would let it fill a gap it does
not fit.

### Decision

A task without a profile of its own gets a coarser one. It first borrows
within its work class: the same class at any complexity, then a template's
base class, then the heaviest profile of that class on the host. Only then
does it get the heaviest profile on the host. A host with no profiles at all
admits as today.

### Consequences

- The forecast never undercounts work it knows nothing about.
- Cost: a new workspace, class or complexity runs at low concurrency until
  it has enough deliveries, even when its tasks are light.

## The operator's ceiling is hard, and admission never cancels work

**Recorded:** 2026-10 · [ORB-15757]

### Context

Ceilings were set after incidents: dk-server-1 reached a load of 85 on 15
cores under a full backlog (2026-09-27), and the Mac's disk filled with
job-run build output (2026-09-28). Cancelling a running task loses its agent
spend and worktree state.

### Decision

`max_active_leaf_runs` stays the hard ceiling, checked before any forecast.
No admission rule cancels, pauses or preempts a running task.

### Consequences

- The worst automatic outcome is the operator's own ceiling.
- Cost: a ceiling set too low caps a host the forecast says has room. The
  readiness view has to show when the ceiling, not the forecast, is what
  binds.

## Task References

- [ORB-15757] — proposed these rules with the auto-concurrency design.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
