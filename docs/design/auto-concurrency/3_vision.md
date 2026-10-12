---
title: Auto-concurrency — Vision
owner: claude
last_updated: 2026-10-11
last_validated: 2026-10-11
status: Draft
feature: auto-concurrency
doc_role: vision
type: design
summary: Open questions the auto-concurrency proposal leaves unanswered (work-unit weighting, revert window, cost in the objective, fleet-wide provider quota, probing) and the control-theory and capacity-planning prior art it borrows from.
tags: [auto-concurrency, drain, capacity, vision]
paths: ["crates/orbit-core/src/adapter/engine_host/v2_host/workspace_auto/**", "crates/orbit-core/src/runtime/host_resource/**"]
related_features: [auto-concurrency, distributed-drain, automation-triggers]
related_artifacts: [ORB-15757]
---

# Auto-concurrency — Vision

[2_design.md](./2_design.md) proposes a per-host concurrency budget that a
congestion-avoidance loop moves between operator bounds, seeded by an offline
estimate from run history. This doc lists what that proposal leaves open and
the prior work it borrows from. Everything here is speculation unless it says
otherwise.

## 1. Open Questions

1. **What is the unit of the host budget?** The proposal counts leaves. A
   `ws_marketing` leaf that edits Markdown and a `ws_orbit` leaf that runs
   `make ci-test-affected` do not load a host equally. The existing
   `cpu_light_leaves` reservation already admits that leaves differ. Should
   the budget be in weighted units, learned per workspace from observed
   resource use, or does the host-level congestion signal make the weighting
   unnecessary in practice?
2. **How long must a change stay landed to count?** Goodput counts landed
   leaves that were not later reverted or repaired. Reverts and
   `regression_from` repair tasks can arrive days later. A long attribution
   window makes the slow loop slower still; a short one counts changes that
   later turn out bad. The phase 1 report should measure the real
   distribution before a window is chosen.
3. **Should cost enter the objective?** Higher concurrency spends more tokens
   per landed change when leaves slow down, fail or rework. Goodput per
   dollar needs per-run spend, which Orbit records only partly
   ([2_design.md §2](./2_design.md#2-what-history-already-records)), and an
   exchange rate between time and money that only the operator can set.
4. **Who owns provider quota?** Provider rate limits are per account and
   shared by every host. A host-local loop only sees them as
   `provider_limit` exclusions after the fact. A fleet-wide provider budget
   would belong to the owner, which already orders admission for pull
   drains, but nothing in v1 measures per-provider headroom.
5. **Does the loop need deliberate probing?** A congestion loop that backs
   off can settle below the true optimum and never revisit it, because
   increases only follow clean intervals. Envoy's adaptive-concurrency
   filter periodically drops to a low limit to re-measure its baseline. For
   leaves that last an hour, a probe costs a lot. Is decaying the soft
   ceiling over time enough, or does the controller need scheduled probes?
6. **Is per-crew concurrency a separate limit?** Opus and Haiku leaves differ
   in provider pressure and duration. A crew-blind host limit may admit too
   many leaves of the expensive crew. The crew window already excludes a
   crew after a provider limit; whether that is sufficient is untested.
7. **Should the owner place work across hosts?** Today each follower pulls
   when it has a free slot. With per-host budgets known, the owner could
   prefer the host with the most headroom. That is placement, which the
   distributed-drain design keeps out of v1 on purpose.
8. **Does the Mac run the online loop at all?** Its disk and fsync behaviour
   make leaf duration noisy. It may be better served by the advisory
   recommendation with a fixed limit.

## 2. Prior Work

### Queueing and capacity planning

- **Little's law** (L = λW) gives throughput as concurrency over cycle time.
  It holds for any stable system regardless of distribution, which is why
  the objective in [2_design.md §1](./2_design.md#1-the-objective-is-goodput)
  is built on it.
- **The Universal Scalability Law** (Gunther) models throughput at
  concurrency N as N / (1 + σ(N−1) + κN(N−1)): σ is contention for a shared
  serial resource, κ is pairwise coherency cost. Its peak is
  N* = √((1−σ)/κ). For Orbit, σ maps to serialized resources (build slots,
  one landing queue) and κ to conflicts between concurrent leaves on one
  base branch, which grow with the number of pairs. Phase 1 fits it per host.

### Congestion control

- **AIMD** (Jacobson, 1988) grows additively and halves on loss. Its
  stability under noisy, delayed feedback is the reason the proposed
  controller is asymmetric.
- **TCP Vegas** (Brakmo and Peterson, 1995) backs off when observed
  round-trip time rises above the base RTT, before loss occurs. Build-step
  latency inflation plays the role of RTT here.
- **Netflix concurrency-limits** applies Vegas, Gradient and AIMD limiters to
  RPC servers and clients. Its gradient limiter, the ratio of a long-window
  to a short-window latency, is the model for the latency signal in
  [2_design.md §4](./2_design.md#4-signals).
- **Envoy's adaptive concurrency filter** combines a gradient controller with
  periodic minimum-RTT measurement windows (open question 5).

### Throughput-maximizing thread pools

- **The .NET thread pool's hill-climbing controller** (Hellerstein, Morrison
  and Eilebrecht, 2008) adjusts worker count to maximize measured
  completions per second, using deliberate perturbation and signal
  filtering to separate the effect of its own changes from workload noise.
  It is the closest precedent for the slow goodput loop, and its main lesson
  is how many samples a decision needs.

## 3. What May Be Distinctive

The control machinery is standard. The setting is not. Most adaptive
concurrency limiters manage requests that last milliseconds and succeed or
fail immediately. An Orbit leaf lasts tens of minutes, varies by an order of
magnitude in size, and can fail long after it finishes: a merge conflict at
landing, a red agent-main, or a revert days later. Rating concurrency by
landed, stayed-landed work rather than completions, and treating merge
conflicts between concurrent agents as the USL coherency term, may be new.
Nothing here has been tried yet.

## 4. References

**Orbit-internal**

- [Auto-concurrency — Design](./2_design.md)
- [Distributed Drain — Overview](../distributed-drain/1_overview.md)
- [Build budget runbook](../../runbooks/build-budget.md)

**External**

- J. D. C. Little, "A Proof for the Queuing Formula: L = λW", *Operations
  Research* 9(3), 1961.
- N. J. Gunther, *Guerrilla Capacity Planning*, Springer, 2007 (Universal
  Scalability Law).
- V. Jacobson, "Congestion Avoidance and Control", SIGCOMM 1988.
- L. S. Brakmo and L. L. Peterson, "TCP Vegas: End to End Congestion
  Avoidance on a Global Internet", *IEEE JSAC* 13(8), 1995.
- Netflix, `concurrency-limits`, https://github.com/Netflix/concurrency-limits
- Envoy, "Adaptive Concurrency" HTTP filter documentation.
- J. L. Hellerstein, V. Morrison and E. Eilebrecht, "Optimizing Concurrency
  Levels in the .NET ThreadPool: A Case Study of Controller Design and
  Implementation", FeBID 2008.

## Task References

- [ORB-15757] — authored this proposal.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
