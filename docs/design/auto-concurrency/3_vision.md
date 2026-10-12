---
title: Auto-concurrency — Vision
owner: claude
last_updated: 2026-10-11
last_validated: 2026-10-11
status: Draft
feature: auto-concurrency
doc_role: vision
type: design
summary: Open questions predictive admission leaves unanswered (step-aligned profiles, macOS sampling, crew-aware profiles, backfill reservations, cross-host placement, measuring whether it helps) and the scheduling and resource-prediction prior work it borrows from.
tags: [auto-concurrency, drain, capacity, vision]
paths: ["crates/orbit-core/src/runtime/host_resource/**", "crates/orbit-core/src/adapter/engine_host/v2_host/admission/auto_admission.rs"]
related_features: [auto-concurrency, distributed-drain]
related_artifacts: [ORB-15757]
---

# Auto-concurrency — Vision

[2_design.md](./2_design.md) proposes admission by forecast host load, built
from per-task resource profiles. This doc lists what that leaves open, and
the prior work it borrows from. Everything here is speculation unless it says
otherwise.

## 1. Open Questions

1. **Minutes or steps?** Profiles are indexed by minutes since admission.
   But a task's heavy phase is tied to a step, such as validation, and when
   that step starts depends on how long implementation took. A profile per
   step, combined with each step's duration distribution, would keep spikes
   sharp instead of averaging them into a hump. It costs a more complex
   forecast. The profiles report ([2_design.md §8](./2_design.md#8-rollout),
   step 2) shows whether minute profiles are too blurred to use.
2. **How should the Mac be sampled?** Without cgroups, a sampler has to sum
   CPU time and resident memory over the worker's process tree, and catch
   short-lived rustc and test processes before they exit. Is a 5 s walk of
   the tree accurate enough, or does it need exit-time accounting?
3. **Do profiles need the crew?** An Opus task and a Haiku task of the same
   complexity differ in length and in how often they build. Adding the crew
   to the key would sharpen profiles, but split the samples thinner.
4. **Reservations instead of a wait timer?** The starvation guard holds
   everything once the queue head has waited 30 minutes. EASY backfilling
   gives the head a reservation at the earliest minute the forecast says it
   fits. It then admits a later task only if that task does not push the
   reservation back. Smaller tasks keep flowing and the head still gets its
   slot.
5. **Should the owner place work?** With forecasts on every host, the owner
   could send a task to the host with the widest gap, instead of whichever
   follower asks first. The distributed-drain design keeps placement out of
   v1 on purpose.
6. **Does it actually help?** The design fills headroom; it does not check
   that more concurrency lands more work. An evaluation should compare tasks
   landed per hour, and their failure rate, before and after enforcement on
   the same host. If conflicts or review are the real bottleneck, the result
   will say so.
7. **What else should be forecast?** Disk I/O, the Mac's fsync stalls and
   provider quota are all resources tasks compete for. Provider quota is
   shared across hosts, so forecasting it belongs to the owner.
8. **Can a task predict its own size?** The number of declared context
   files, the diff size of similar past tasks, or the pilot's estimate could
   pick a profile more precisely than the complexity label alone.
9. **Are there more work classes?** Task type (bug, feature, refactor,
   chore) and changes that touch only documentation may have shapes of their
   own: a docs-only change validates without compiling. Each new class
   divides the samples further, so a class should earn its place by a
   visibly different profile in the report.

## 2. Prior Work

### Usage-based resource prediction

- **Kubernetes Vertical Pod Autoscaler.** Its recommender sets container
  resource requests from a decaying histogram of observed usage. It is the
  same idea of learning a workload's footprint from history rather than
  trusting a declared one, applied per container instead of over a task's
  lifetime.
- **Autopilot** (Rzadca et al., 2020). Google sets job resource limits from
  historical usage with moving-window and model-based recommenders. It shows
  that simple per-minute statistics over a recent window are usually
  enough.
- **Resource Central** (Cortez et al., 2017). Azure predicts a VM's
  utilization from the history of similar VMs, and uses it to decide placement
  and oversubscription. That is the closest precedent for admitting on a
  forecast instead of current load.

### Batch scheduling

- **EASY backfilling** (Lifka, 1995; Mu'alem and Feitelson, 2001). It fills
  holes in a schedule with jobs that fit, without delaying the reservation
  of the job at the head of the queue. It is the upgrade path for the
  starvation guard (question 4). The second paper's finding that user
  runtime estimates are poor is a warning for complexity labels too.

### Congestion control

- **Slow start** (Jacobson, 1988). Probing capacity one step at a time and
  backing off on loss is the reactive alternative this design moves beyond.
  The resource throttle keeps that role as the backstop.

## 3. What May Be Distinctive

Batch schedulers and container autoscalers predict a workload's total
footprint. An Orbit task's footprint has a strong shape over time: light
while the agent reads and plans, heavy when it builds and validates. That
shape repeats by repository and task size. Admitting on the shape, so a
task's light phase overlaps another's heavy one, may be new for coding-agent
orchestration. The forecasting itself is standard. Nothing here has been
tried yet.

## 4. References

**Orbit-internal**

- [Auto-concurrency — Design](./2_design.md)
- [Distributed Drain — Overview](../distributed-drain/1_overview.md)

**External**

- Kubernetes, "Vertical Pod Autoscaler", kubernetes/autoscaler repository.
- K. Rzadca et al., "Autopilot: Workload Autoscaling at Google", EuroSys 2020.
- E. Cortez et al., "Resource Central: Understanding and Predicting Workloads
  for Improved Resource Management in Large Cloud Platforms", SOSP 2017.
- D. Lifka, "The ANL/IBM SP Scheduling System", JSSPP 1995.
- A. W. Mu'alem and D. G. Feitelson, "Utilization, Predictability,
  Workloads, and User Runtime Estimates in Scheduling the IBM SP2 with
  Backfilling", *IEEE TPDS* 12(6), 2001.
- V. Jacobson, "Congestion Avoidance and Control", SIGCOMM 1988.

## Task References

- [ORB-15757] — authored this proposal.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
