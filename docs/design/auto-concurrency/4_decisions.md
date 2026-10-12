---
title: Auto-concurrency — Decisions
owner: claude
last_updated: 2026-10-11
last_validated: 2026-10-11
status: Draft
feature: auto-concurrency
doc_role: decisions
type: design
summary: Proposed standing rules for automatic drain concurrency — rate by landed goodput, budget per host, back off fast and grow only on saturated demand, never exceed the operator's bounds, never cancel to shed load.
tags: [auto-concurrency, drain, capacity, decisions]
paths: ["crates/orbit-core/src/adapter/engine_host/v2_host/workspace_auto/**"]
related_features: [auto-concurrency, distributed-drain]
related_artifacts: [ORB-15757]
---

# Auto-concurrency — Decisions

Record non-obvious decisions here by title. Task references carry provenance; superseded decisions remain in place so their original reasoning stays legible. See [CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule and required `Cost:` line.

Nothing here is implemented yet. Every entry is a proposed Door 2 rule: it
governs choices the implementation tasks have not made yet. Each is open to
the operator's revision while the folder is `Draft`.

## Rate concurrency by landed goodput, never by completions

**Recorded:** 2026-10 · [ORB-15757]

### Context

The obvious throughput measure is finished leaves per hour. It rewards the
wrong things: a leaf that fails fast, opens a PR that conflicts, or lands a
change that is later reverted all count as completions. Raising concurrency
tends to increase exactly those outcomes.

### Decision

Any measure that rates a concurrency level, whether offline, advisory or in
the online loop, counts only work that landed and stayed landed, weighted by
its complexity class's baseline duration
([2_design.md §1](./2_design.md#1-the-objective-is-goodput)). Completion
counts, PR counts and admission counts may be shown, but never optimized.

### Consequences

- Fast failures, landing conflicts and reverts lower the rating of the level
  that produced them.
- Cost: goodput is only known after landing and the revert window, hours
  after admission. That makes it too slow for the fast loop, which has to
  steer on congestion proxies that may not track it. It also needs revert
  and repair attribution that Orbit does not persist in a queryable form
  today.

## Concurrency budgets belong to the host; workspaces get caps, not tuners

**Recorded:** 2026-10 · [ORB-15757]

### Context

Concurrency is set per drain today, so the natural extension is one tuner
per workspace on each host. But every drain on a host competes for the same
CPU, memory, disk and provider quota. Two independent tuners each read the
other's load as noise and keep correcting against each other.

### Decision

The quantity the controller tunes is one budget per host, shared by every
drain on it. A workspace contributes a cap, learned from contention on its
own base branch (conflicts, rework). This cap is enforced where that
workspace's admissions are ordered, the owner for pull drains. A drain's
effective limit is its share of the host budget, bounded by its workspace
cap. Any future per-something limit (per crew, per provider) is a cap on the
host budget, not a second tuner.

### Consequences

- One feedback loop per host. Its signals are host-wide, so they are not
  confounded by the host's other drains.
- Cost: drains on one host need a shared coordination point to split the
  budget, which they do not have today, and an allocation rule. A bad
  allocation lets a deep backlog in one workspace starve another.

## Back off on any congestion signal; grow only on evidence of saturated demand

**Recorded:** 2026-10 · [ORB-15757]

### Context

Feedback is slow and noisy. An increase that overloads the host costs every
running leaf, and those leaves cannot be recovered. A level that is too low
only costs some idle capacity. And when the ready backlog is shorter than the
limit, raising the limit changes nothing. A loop that treats a quiet interval
as permission to grow lets the limit drift to the maximum with no evidence.

### Decision

The controller is asymmetric. It cuts the limit by a multiple on any single
congestion signal. It raises it by at most one, and only after an interval in
which every slot was filled, ready work was still waiting, and no congestion
signal fired. When signals disagree, the conservative reading wins.

### Consequences

- The limit stays where the host was last seen to be comfortable, and
  overload is short.
- Cost: it settles below the true optimum when the signals are noisy or
  oversensitive, and climbs back slowly after a transient such as a
  provider outage. A level that was once cut can take hours to recover.

## The operator's bounds are hard, and a manual resize wins

**Recorded:** 2026-10 · [ORB-15757]

### Context

The current limits were set after incidents: dk-server-1 reached a load of
85 on 15 cores under a full backlog (2026-09-27), and the Mac's disk filled
with job-run build output (2026-09-28). An optimizer that explores above
them can repeat those failures, and it does so while nobody is watching.

### Decision

Auto mode only moves the limit between the operator's minimum and maximum.
It never explores above the maximum, even when its estimate says the optimum
is there. A manual resize on a drain switches that drain to manual for the
rest of its window. Every automatic change is recorded with its reason and
the signals behind it.

### Consequences

- The worst automatic outcome is the operator's own maximum.
- Cost: a maximum set too low hides the optimum from the controller. The
  advisory report has to say when the ceiling binds, or nobody learns that
  raising it would help.

## Lowering the limit never cancels running work

**Recorded:** 2026-10 · [ORB-15757]

### Context

Shedding load immediately would mean cancelling running leaves. A cancelled
leaf loses its agent spend and its worktree state, and it may leave a claim
to settle.

### Decision

A lower limit only stops new admission until the number of active leaves
falls below it. No controller action cancels, pauses or preempts a leaf.

### Consequences

- A decrease never destroys work.
- Cost: overload persists until enough leaves finish, up to one leaf
  duration. The resource throttle's admission hold is the only protection
  during that time.

## Task References

- [ORB-15757] — proposed these rules with the auto-concurrency design.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
